use std::collections::HashMap;

use wafer_block::db::{ListOptions, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, InputStream, Message, OutputStream};

use super::{
    contracts::{
        AdminRoleDeleteResponse, AdminRoleListResponse, AdminRoleUpdateResponse, AdminRoleView,
        CreateRoleRequest, UpdateRoleRequest,
    },
    logs::audit_log,
};
use crate::{
    blocks::{auth::bump_auth_version, crud},
    db_read::{self, Bound},
    http::{err_bad_request, err_conflict, err_forbidden, err_not_found, ok_json},
    platform_state::user_roles::{self, Assigned},
    util::{json_map, RecordExt},
};

/// Role definitions table (one row per named role).
pub(crate) const ROLES_TABLE: &str = "impresspress__admin__roles";

/// Per-role permission rows (resource + actions tuples).
pub(crate) const PERMISSIONS_TABLE: &str = "impresspress__admin__permissions";

/// `GET /b/admin/api/iam/roles`.
pub(super) async fn handle_list_roles(ctx: &dyn Context) -> OutputStream {
    let opts = ListOptions {
        sort: vec![SortField {
            field: "name".to_string(),
            desc: false,
        }],
        limit: Some(1000),
        ..Default::default()
    };
    match db::list(ctx, ROLES_TABLE, &opts).await {
        // Project onto the closed `AdminRoleView` field list. Besides pinning
        // the published field set, this normalizes `permissions`: the column is
        // JSON-encoded TEXT that the SQLite backend sniffs back into an array
        // while Postgres/D1 return the raw string, so the untyped response had
        // no single shape a schema could describe.
        Ok(result) => ok_json(&AdminRoleListResponse::from_record_list(&result)),
        Err(e) => crud::db_error_internal(e, "Database error"),
    }
}

/// `POST /b/admin/api/iam/roles`.
pub(super) async fn handle_create_role(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body: CreateRoleRequest = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    // Validation, audit-log write, and the create live in the shared ops layer.
    match super::ops::create_role(
        ctx,
        msg,
        &body.name,
        body.description.as_deref(),
        body.permissions,
    )
    .await
    {
        // Same projection as the list: the ops layer returns the raw
        // `db::Record`, whose `{id, data: {…}}` envelope and backend-dependent
        // `permissions` encoding are exactly what `AdminRoleView` exists to
        // normalize away. Echoing it here would publish a second shape for
        // the same row.
        Ok(record) => ok_json(&AdminRoleView::from_record(&record)),
        Err(out) => out,
    }
}

/// `PATCH /b/admin/api/iam/roles/{id}`. `{id}` is read only as the route
/// table bound it.
pub(super) async fn handle_update_role(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    let id = match crud::path_id(msg, "Role") {
        Ok(value) => value,
        Err(response) => return response,
    };

    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    // Typed rather than a `HashMap` peek plus a per-branch key whitelist: the
    // published schema names exactly these three fields, and a `permissions`
    // that is not an array of strings is refused here instead of being
    // written to the column as whatever JSON arrived.
    let body: UpdateRoleRequest = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    // Protect system roles from name changes (renaming "admin" would break
    // auth). The guard read must fail closed: success / not-found / infra
    // error are matched explicitly, and an infra error rejects the mutation
    // instead of silently falling through to the unprotected update below
    // (the old `if let Ok(existing) =` swallowed any non-success result,
    // including a transient DB error, as "not a system role").
    let existing = match db::get(ctx, ROLES_TABLE, id).await {
        Ok(record) => record,
        Err(e) => return crud::db_error(e, "Role not found", "Database error"),
    };

    let is_system = existing.bool_field("is_system");
    if is_system && body.name.is_some() {
        return err_forbidden("Cannot rename system roles");
    }

    // `user_roles.role` stores the role NAME, not its id (`fetch_roles` reads
    // the `role` column; `handle_assign_role` writes `body.role`). A rename
    // that does not carry the grants with it leaves every assignment naming a
    // role that no longer exists, so the grant silently stops matching.
    let old_name = existing.str_field("name").to_string();
    let rename_to = body
        .name
        .as_deref()
        .filter(|name| *name != old_name)
        .map(str::to_string);

    let mut data = HashMap::new();
    if let Some(name) = body.name {
        data.insert("name".to_string(), serde_json::Value::String(name));
    }
    if let Some(description) = body.description {
        data.insert(
            "description".to_string(),
            serde_json::Value::String(description),
        );
    }
    if let Some(permissions) = body.permissions {
        data.insert("permissions".to_string(), serde_json::json!(permissions));
    }
    crate::util::stamp_updated(&mut data);
    // The role row is written first, and on its own: `roles.name` is unique,
    // so a rename onto a name another role holds is refused right here,
    // before a single grant has moved or a single token been invalidated.
    // What makes it unique everywhere is the
    // `impresspress__admin__roles_name_uniq` index migration 001 creates: the
    // column's own `UNIQUE` exists only on a table that migration created,
    // not on one that predates it. The 409 below depends on that index —
    // on such a table without it, the update lands and two roles share the
    // name. The refused write says so itself (`AlreadyExists`, which
    // `crud::taken_key_or_db_error` answers as this 409), so there is no
    // read beforehand for a competing rename to slip past.
    let record = match db::update(ctx, ROLES_TABLE, id, data).await {
        Ok(record) => record,
        Err(e) if e.code == wafer_run::ErrorCode::NotFound => {
            return crud::db_error(e, "Role not found", "Database error")
        }
        Err(e) => match &rename_to {
            Some(new_name) => {
                return crud::taken_key_or_db_error(
                    e,
                    crud::TakenKey::new("role", "name", new_name),
                    "Database error",
                )
            }
            None => return crud::db_error(e, "Role not found", "Database error"),
        },
    };

    // From here the update HAS happened: the row carries the new values. A
    // cascade that stops part-way is not a failed update, so it is answered
    // 200 with the role as it now is, and the audit row is written either way
    // — an error here would tell the caller the rename did not happen, and
    // leave a mutation that did happen unaudited.
    let mut resource = format!("roles/{id}");
    let mut warning = None;
    if let Some(new_name) = &rename_to {
        if let Err(stopped) = cascade_role_rename(ctx, &old_name, new_name).await {
            tracing::error!(
                role_id = %id,
                old_name = %old_name,
                new_name = %new_name,
                error = %stopped.error,
                "role renamed, but its grants did not all follow: {}",
                stopped.audit_detail()
            );
            resource = format!(
                "roles/{id} (renamed from {old_name} to {new_name}; {})",
                stopped.audit_detail()
            );
            warning = Some(stopped.warning(&old_name, new_name));
        }
    }

    audit_log(
        ctx,
        msg.user_id(),
        "role.update",
        &resource,
        msg.remote_addr(),
    )
    .await;

    // Same projection as list/create, for the same reason.
    ok_json(&AdminRoleUpdateResponse {
        role: AdminRoleView::from_record(&record),
        warning,
    })
}

/// Where [`cascade_role_rename`] stopped, when it did not finish.
struct CascadeStopped {
    /// How many grants it had read, or `None` when the read itself failed.
    total: Option<usize>,
    /// How many grants it had moved onto the new name.
    moved: usize,
    /// The holder whose sessions it could not invalidate, when that is what
    /// stopped it. Their grant has moved; a token they already hold still
    /// carries the old name until it expires.
    not_invalidated: Option<String>,
    error: wafer_run::WaferError,
}

impl CascadeStopped {
    /// The step it stopped at and how far it got, for the audit row and the
    /// log line.
    fn audit_detail(&self) -> String {
        match (self.total, &self.not_invalidated) {
            (None, _) => "the grant cascade could not read the grants; none moved".to_string(),
            (Some(total), None) => format!(
                "the grant cascade stopped after moving {} of {total} grants",
                self.moved
            ),
            (Some(total), Some(user_id)) => format!(
                "the grant cascade moved {} of {total} grants, then could not invalidate the \
                 sessions of {user_id}",
                self.moved
            ),
        }
    }

    /// What the caller is told: the rename happened, which grants did not
    /// follow it, and how to move them.
    fn warning(&self, old_name: &str, new_name: &str) -> String {
        let finish = format!(
            "Rename the role back to \"{old_name}\" and then to \"{new_name}\" again to move \
             the rest."
        );
        let left = |left: usize| {
            format!(
                "{left} still name \"{old_name}\", which no role is called any more, so their \
                 holders do not hold this role."
            )
        };
        match (self.total, &self.not_invalidated) {
            (None, _) => format!(
                "Role renamed to \"{new_name}\", but its grants could not be read, so none of \
                 them moved: every grant of \"{old_name}\" still names it, which no role is \
                 called any more. {finish}"
            ),
            (Some(total), None) => format!(
                "Role renamed to \"{new_name}\", but only {} of its {total} grants moved. {} \
                 {finish}",
                self.moved,
                left(total - self.moved)
            ),
            (Some(total), Some(user_id)) if self.moved == total => format!(
                "Role renamed to \"{new_name}\" and all {total} of its grants moved, but the \
                 sessions of user {user_id} could not be invalidated: a token they already \
                 hold carries \"{old_name}\" until it expires."
            ),
            (Some(total), Some(user_id)) => format!(
                "Role renamed to \"{new_name}\", but only {} of its {total} grants moved, and \
                 the sessions of user {user_id} could not be invalidated: a token they already \
                 hold carries \"{old_name}\" until it expires. {} {finish}",
                self.moved,
                left(total - self.moved)
            ),
        }
    }
}

/// Carry a role rename onto every `user_roles` row naming the old value, and
/// invalidate the affected users' access tokens.
///
/// The grants store the role name, so this is what keeps them pointing at the
/// role they were granted. The auth-version bump is the same reasoning as
/// `handle_assign_role`'s: the set of roles a live JWT was minted with has
/// changed, so it must stop authenticating.
///
/// It runs only once the role row carries the new name, so a rename the
/// unique index refuses never reaches it. A grant whose holder already has
/// one naming the new value is merged by [`user_roles::rename_role`], not
/// refused, so that is not a collision this has to foresee.
///
/// Not atomic. The role update and each grant's rewrite and bump are
/// separate writes, and there is no transaction primitive to put them in: a
/// failure part-way leaves the role under its new name, the grants before
/// the failing one moved, and the rest still naming the old one. It returns
/// where it stopped, which `handle_update_role` audits and reports. Repeating
/// the same PATCH does not finish the job — the name no longer differs, so
/// no cascade runs. Renaming the role back and then forward again does,
/// since each rename carries every grant naming the name it leaves.
async fn cascade_role_rename(
    ctx: &dyn Context,
    old_name: &str,
    new_name: &str,
) -> Result<(), CascadeStopped> {
    let grants = match user_roles::list_by_role(ctx, old_name).await {
        Ok(rows) => rows,
        Err(error) => {
            return Err(CascadeStopped {
                total: None,
                moved: 0,
                not_invalidated: None,
                error,
            })
        }
    };

    for (moved, grant) in grants.iter().enumerate() {
        if let Err(error) = user_roles::rename_role(ctx, grant, new_name).await {
            return Err(CascadeStopped {
                total: Some(grants.len()),
                moved,
                not_invalidated: None,
                error,
            });
        }

        if let Err(error) = bump_auth_version(ctx, &grant.user_id).await {
            return Err(CascadeStopped {
                total: Some(grants.len()),
                moved: moved + 1,
                not_invalidated: Some(grant.user_id.clone()),
                error,
            });
        }
    }
    Ok(())
}

/// `DELETE /b/admin/api/iam/roles/{id}`. `{id}` is read only as the route
/// table bound it.
pub(super) async fn handle_delete_role(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = msg.var("id");
    // System-role guard, grant revocation, delete, and audit-log write live
    // in the shared ops layer. Every `Ok` is a role that is gone, including
    // one whose late-grant revocation pass failed — the ops layer has logged
    // and audited that, and an error here would say the role survived.
    match super::ops::delete_role(ctx, msg, id).await {
        Ok(_) => ok_json(&AdminRoleDeleteResponse { deleted: true }),
        Err(out) => out,
    }
}

/// `GET /b/admin/api/iam/permissions`.
pub(super) async fn handle_list_permissions(ctx: &dyn Context) -> OutputStream {
    match db_read::list_bounded(
        ctx,
        PERMISSIONS_TABLE,
        vec![],
        Bound::Curated(
            "the IAM permission catalogue — seeded by migration, extended only by an admin",
        ),
    )
    .await
    {
        Ok(records) => {
            let total_count = records.len() as i64;
            ok_json(&db::RecordList {
                records,
                total_count,
                page: 1,
                page_size: total_count,
            })
        }
        Err(e) => crud::db_error_internal(e, "Database error"),
    }
}

/// `POST /b/admin/api/iam/permissions`.
pub(super) async fn handle_create_permission(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    #[derive(serde::Deserialize)]
    struct Req {
        name: String,
        resource: String,
        actions: Vec<String>,
    }
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body: Req = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };
    let mut data = json_map(serde_json::json!({
        "name": body.name,
        "resource": body.resource,
        "actions": body.actions
    }));
    crate::util::stamp_created(&mut data);
    match db::create(ctx, PERMISSIONS_TABLE, data).await {
        Ok(record) => {
            // The permission catalogue is what roles are written against, so
            // adding an entry is an admin mutation like every other one on
            // this page — audited with the same writer, after the row lands.
            audit_log(
                ctx,
                msg.user_id(),
                "permission.create",
                &format!("permissions/{}", record.id),
                msg.remote_addr(),
            )
            .await;
            ok_json(&record)
        }
        // `permissions.name` is UNIQUE: a second permission of the same name is
        // a 409, not a 500 — the same classification `ops::create_variable` and
        // `ops::create_role` make, through the same helper. It also stops this
        // line flattening a WRAP refusal to 500, which is what
        // `crud::db_error_internal` inside the helper takes care of.
        Err(e) => crate::blocks::crud::taken_key_or_db_error(
            e,
            crate::blocks::crud::TakenKey::new("permission", "name", &body.name),
            "Database error",
        ),
    }
}

/// `DELETE /b/admin/api/iam/permissions/{id}`. `{id}` is read only as the
/// route table bound it.
pub(super) async fn handle_delete_permission(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match crud::path_id(msg, "Permission") {
        Ok(value) => value,
        Err(response) => return response,
    };
    match db::delete(ctx, PERMISSIONS_TABLE, id).await {
        Ok(()) => {
            audit_log(
                ctx,
                msg.user_id(),
                "permission.delete",
                &format!("permissions/{id}"),
                msg.remote_addr(),
            )
            .await;
            ok_json(&serde_json::json!({"deleted": true}))
        }
        Err(e) => crud::db_error(e, "Permission not found", "Database error"),
    }
}

/// `GET /b/admin/api/iam/user-roles`.
pub(super) async fn handle_list_user_roles(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.query("user_id").to_string();
    // Unfiltered, this lists a table that grows with the user base, so the
    // read is capped. `total_count` then has to come from the database rather
    // than from `records.len()`: a `total_count` copied off a capped page
    // would tell the client the grant list is complete when it is a prefix.
    let (rows, total_count) = if user_id.is_empty() {
        match user_roles::list_all(ctx).await {
            Ok(capped) => {
                let total = if capped.truncated {
                    match user_roles::count_all(ctx).await {
                        Ok(total) => total,
                        Err(e) => return crud::db_error_internal(e, "Database error"),
                    }
                } else {
                    capped.rows.len() as i64
                };
                (capped.rows, total)
            }
            Err(e) => return crud::db_error_internal(e, "Database error"),
        }
    } else {
        match user_roles::list_for_user(ctx, &user_id).await {
            Ok(rows) => {
                let total = rows.len() as i64;
                (rows, total)
            }
            Err(e) => return crud::db_error_internal(e, "Database error"),
        }
    };
    // Echoed in the `{id, data}` record envelope this endpoint has
    // always published; declared without a schema until it is typed.
    let records: Vec<db::Record> = rows
        .iter()
        .map(|row| db::Record {
            id: row.id.clone(),
            data: row.to_data(),
        })
        .collect();
    let page_size = records.len() as i64;
    ok_json(&db::RecordList {
        records,
        total_count,
        page: 1,
        page_size,
    })
}

/// `POST /b/admin/api/iam/user-roles`.
pub(super) async fn handle_assign_role(
    ctx: &dyn Context,
    msg: &Message,
    input: InputStream,
) -> OutputStream {
    #[derive(serde::Deserialize)]
    struct Req {
        user_id: String,
        role: String,
    }
    let raw = match input.collect_to_bytes().await {
        Ok(bytes) => bytes,
        Err(e) => return OutputStream::error(e),
    };
    let body: Req = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    // Only a defined role can be granted. A grant names its role by string,
    // and a grant naming no role is not inert: the auth block puts it in the
    // holder's token `roles` claim regardless, and a role later created under
    // that name re-attaches to it. It is also what lets a grant outlive
    // `ops::delete_role`, whose second revocation pass relies on no assign
    // getting past this check once the role row is gone.
    match super::ops::role_name_taken(ctx, &body.role).await {
        Ok(true) => {}
        Ok(false) => {
            return err_bad_request(&format!(
                "No role named \"{}\" exists. Create the role first.",
                body.role
            ))
        }
        Err(e) => return crud::db_error_internal(e, "Database error"),
    }

    let assigned = format!("users/{}/roles/{}", body.user_id, body.role);
    match user_roles::assign(ctx, &body.user_id, &body.role, msg.user_id()).await {
        Ok(Assigned::AlreadyAssigned) => err_conflict("Role already assigned to user"),
        Ok(Assigned::Created(row)) => {
            // P2c: a role grant is a security-relevant change — bump the
            // affected user's auth_version so any already-issued access JWT
            // (minted with the old role set) is invalidated instead of
            // keeping its stale `roles` claim until natural expiry. The row
            // has already landed, so a failed bump must not read as success.
            if let Err(e) = bump_auth_version(ctx, &body.user_id).await {
                tracing::error!(
                    user_id = %body.user_id,
                    error = %e,
                    "role assigned but auth_version bump failed"
                );
                return crud::db_error_internal(e, "Role assigned but session invalidation failed");
            }
            // Audit-log like every other admin mutation (this JSON path used to
            // write zero audit rows).
            audit_log(
                ctx,
                msg.user_id(),
                "user_role.assign",
                &assigned,
                msg.remote_addr(),
            )
            .await;
            // Echoed in the `{id, data}` record envelope this endpoint has
            // always published; declared without a schema until it is typed.
            ok_json(&db::Record {
                id: row.id.clone(),
                data: row.to_data(),
            })
        }
        Err(e) => crud::db_error_internal(e, "Database error"),
    }
}

/// `DELETE /b/admin/api/iam/user-roles/{id}`. `{id}` is read only as the
/// route table bound it.
pub(super) async fn handle_remove_role(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let id = match crud::path_id(msg, "User-role") {
        Ok(value) => value,
        Err(response) => return response,
    };

    // Prevent admins from removing their own admin role (self-lockout).
    // Also captures the affected user id so a successful removal can bump
    // their auth_version (P2c) below.
    let role_user = match user_roles::get(ctx, id).await {
        Ok(Some(grant)) => {
            if grant.user_id == msg.user_id() && grant.role == "admin" {
                return err_bad_request("Cannot remove your own admin role");
            }
            grant.user_id
        }
        Ok(None) => {
            return err_not_found("User-role assignment not found");
        }
        Err(e) => {
            return crud::db_error_internal(e, "Database error");
        }
    };

    // P2c: role removal (demotion) is exactly the change this mechanism
    // exists for — bump so a JWT minted with the removed role stops
    // authenticating as that role immediately rather than at its natural
    // expiry. Bumped BEFORE the removal as well as after, for the reason
    // `ops::revoke_every_grant_of` gives: if a bump after the removal were the
    // only one and it failed, the grant would be gone and a retry would answer
    // 404, leaving the holder's live token carrying the role until it
    // expires. A failure of this first bump leaves the grant in place to
    // retry; the second closes a refresh landing between the two.
    if let Err(e) = bump_auth_version(ctx, &role_user).await {
        tracing::error!(
            user_id = %role_user,
            error = %e,
            "role not removed: auth_version bump failed"
        );
        return crud::db_error_internal(e, "Role not removed: session invalidation failed");
    }

    match user_roles::remove(ctx, id).await {
        Ok(()) => {
            if let Err(e) = bump_auth_version(ctx, &role_user).await {
                tracing::error!(
                    user_id = %role_user,
                    error = %e,
                    "role removed but auth_version bump failed"
                );
                return crud::db_error_internal(e, "Role removed but session invalidation failed");
            }
            audit_log(
                ctx,
                msg.user_id(),
                "user_role.remove",
                &format!("user_roles/{id}"),
                msg.remote_addr(),
            )
            .await;
            ok_json(&serde_json::json!({"deleted": true}))
        }
        Err(e) => crud::db_error(e, "User-role assignment not found", "Database error"),
    }
}

pub async fn seed_defaults(ctx: &dyn Context) {
    let count = db::count(ctx, ROLES_TABLE, &[]).await.unwrap_or(0);
    if count > 0 {
        return;
    }

    let now = crate::util::now_rfc3339();
    for (name, desc) in &[
        ("admin", "Full access to all resources"),
        ("user", "Standard user access"),
    ] {
        let data = json_map(serde_json::json!({
            "name": name,
            "description": desc,
            "is_system": true,
            "created_at": now,
            "permissions": []
        }));
        if let Err(e) = db::create(ctx, ROLES_TABLE, data).await {
            tracing::warn!("Failed to seed default role '{name}': {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use wafer_block::db::{Filter, FilterOp};
    use wafer_run::{BlockInfo, ErrorCode, WaferError};

    use super::*;
    use crate::{
        blocks::admin::test_support::routed,
        test_support::{admin_msg, audit_rows, output_is_error, output_json, TestContext},
    };

    /// `PATCH /b/admin/api/iam/roles/{role_id}`, with `{id}` bound by the
    /// table the way it is on the wire.
    fn update_role_msg(role_id: &str) -> Message {
        routed(admin_msg(
            "update",
            &format!("/b/admin/api/iam/roles/{role_id}"),
        ))
    }

    /// Wraps a `TestContext` and turns every `db::get` call
    /// (`ServiceOp::DATABASE_GET`, wire kind `"database.get"`) into a
    /// simulated infra failure while every other database op (list, update,
    /// count, ...) passes through untouched. Used to reproduce "the DB read
    /// used for the system-role guard fails transiently" without needing a
    /// fake database backend — everything else in the fixture is the real
    /// in-memory SQLite `TestContext`.
    #[derive(Clone)]
    struct FailingGetContext {
        inner: TestContext,
    }

    #[async_trait]
    impl Context for FailingGetContext {
        fn check_resource_access(
            &self,
            resource: &str,
            resource_type: wafer_run::ResourceType,
            access: wafer_block::ResourceAccess,
        ) -> Result<(), WaferError> {
            self.inner
                .check_resource_access(resource, resource_type, access)
        }

        fn resource_access_admitted(
            &self,
            resource: &str,
            resource_type: wafer_run::ResourceType,
            access: wafer_block::ResourceAccess,
        ) -> bool {
            self.inner
                .resource_access_admitted(resource, resource_type, access)
        }

        async fn call_block(&self, name: &str, msg: Message, input: InputStream) -> OutputStream {
            if name == "wafer-run/database" && msg.action() == "database.get" {
                return OutputStream::error(WaferError::new(
                    ErrorCode::Internal,
                    "simulated database outage",
                ));
            }
            self.inner.call_block(name, msg, input).await
        }

        fn is_cancelled(&self) -> bool {
            self.inner.is_cancelled()
        }

        fn registered_blocks(&self) -> &[BlockInfo] {
            self.inner.registered_blocks()
        }

        fn config_get(&self, key: &str) -> Option<&str> {
            self.inner.config_get(key)
        }

        fn clone_arc(&self) -> Arc<dyn Context> {
            Arc::new(self.clone())
        }
    }

    /// The roles list publishes exactly `AdminRoleView`'s fields, and
    /// `permissions` arrives as an array of strings.
    ///
    /// The array is the part worth pinning: the column is JSON-encoded TEXT,
    /// and only the SQLite backend decodes it on read. Echoing the row would
    /// make the published `array of string` schema false on Postgres and D1,
    /// where the same column comes back as a string.
    #[tokio::test]
    async fn list_roles_publishes_exactly_the_contract_fields() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let msg = crate::test_support::admin_msg("create", "/b/admin/api/iam/roles");
        let created = super::super::ops::create_role(
            &ctx,
            &msg,
            "editor",
            Some("Can edit content"),
            Some(vec!["posts.write".to_string(), "posts.read".to_string()]),
        )
        .await;
        assert!(created.is_ok(), "create role should succeed");

        let body = crate::test_support::output_json(handle_list_roles(&ctx).await).await;

        let editor = body["records"]
            .as_array()
            .expect("records array")
            .iter()
            .find(|r| r["name"] == serde_json::json!("editor"))
            .expect("the created role is listed");

        let mut got: Vec<&str> = editor
            .as_object()
            .expect("role object")
            .keys()
            .map(String::as_str)
            .collect();
        got.sort_unstable();
        assert_eq!(
            got,
            vec![
                "created_at",
                "description",
                "id",
                "is_system",
                "name",
                "permissions",
                "updated_at"
            ],
            "the wire field set must equal AdminRoleView's"
        );

        assert_eq!(
            editor["permissions"],
            serde_json::json!(["posts.write", "posts.read"]),
            "permissions must be an array of strings on every backend"
        );
        assert_eq!(editor["is_system"], serde_json::json!(false));
    }

    /// Seed a real system role (`is_system: true`) via the shared
    /// `seed_defaults` path and return its row id.
    async fn seed_system_role(ctx: &dyn Context) -> String {
        seed_defaults(ctx).await;
        let records = db_read::list_every(
            ctx,
            ROLES_TABLE,
            vec![Filter {
                field: "name".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!("admin"),
            }],
        )
        .await
        .expect("list seeded admin role");
        records
            .into_iter()
            .next()
            .expect("admin role was seeded")
            .id
    }

    fn body_input(json: serde_json::Value) -> InputStream {
        InputStream::from_bytes(serde_json::to_vec(&json).unwrap())
    }

    /// Define a role through the create handler and return its row id — a
    /// grant can only name a defined role.
    async fn define_role(ctx: &TestContext, name: &str) -> String {
        output_json(
            handle_create_role(
                ctx,
                &admin_msg("create", "/b/admin/api/iam/roles"),
                body_input(serde_json::json!({ "name": name })),
            )
            .await,
        )
        .await["id"]
            .as_str()
            .expect("created role id")
            .to_string()
    }

    /// The system-role guard on the delete path must fail closed, exactly as
    /// the update path does. A transient read error previously fell through
    /// the `if let Ok(role)` and deleted the row — and this endpoint is now
    /// declared, schema-bearing and agent-reachable.
    #[tokio::test]
    async fn delete_role_rejects_deletion_when_guard_read_errors() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let role_id = seed_system_role(&ctx).await;
        let failing = FailingGetContext { inner: ctx };

        let out = super::super::ops::delete_role(
            &failing,
            &admin_msg("delete", "/b/admin/api/iam/roles"),
            &role_id,
        )
        .await;

        match out {
            Err(stream) => {
                assert!(
                    output_is_error(stream, "Internal").await,
                    "a failed guard read must be reported, not treated as \
                     'not a system role'"
                );
            }
            Ok(_) => panic!("delete succeeded while the system-role guard read was failing"),
        }

        // The row must still be there: the mutation must not have run.
        let still_there = db::get(&failing.inner, ROLES_TABLE, &role_id).await;
        assert!(
            still_there.is_ok(),
            "the system role was deleted despite the guard read failing"
        );
    }

    /// `user_roles.role` stores the role NAME, so renaming a role definition
    /// without cascading silently orphans every grant: the assignment rows
    /// keep naming a role that no longer exists, and every auth check that
    /// reads them stops matching.
    #[tokio::test]
    async fn update_role_rename_cascades_to_its_assignments() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);

        let created = output_json(
            handle_create_role(
                &ctx,
                &admin_msg("create", "/b/admin/api/iam/roles"),
                body_input(serde_json::json!({ "name": "editor", "permissions": ["posts.write"] })),
            )
            .await,
        )
        .await;
        let role_id = created["id"].as_str().expect("created role id").to_string();

        // Grant it to a user, the way the admin UI does.
        let assigned = handle_assign_role(
            &ctx,
            &admin_msg("create", "/b/admin/api/iam/user-roles"),
            body_input(serde_json::json!({ "user_id": "user_1", "role": "editor" })),
        )
        .await;
        assert!(
            assigned.collect_buffered().await.is_ok(),
            "seeding the role assignment must succeed"
        );

        let out = handle_update_role(
            &ctx,
            &update_role_msg(&role_id),
            body_input(serde_json::json!({ "name": "editor-v2" })),
        )
        .await;
        assert!(out.collect_buffered().await.is_ok(), "rename must succeed");

        let rows = user_roles::list_for_user(&ctx, "user_1")
            .await
            .expect("list assignments");
        let names: Vec<&str> = rows.iter().map(|r| r.role.as_str()).collect();
        assert_eq!(
            names,
            vec!["editor-v2"],
            "the grant must follow the rename, or it names a role that no \
             longer exists"
        );
    }

    /// Every other role mutation writes an audit row; a rename — which
    /// invalidates every grant naming the old value — wrote none.
    #[tokio::test]
    async fn update_role_writes_an_audit_row() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let created = output_json(
            handle_create_role(
                &ctx,
                &admin_msg("create", "/b/admin/api/iam/roles"),
                body_input(serde_json::json!({ "name": "auditor", "permissions": [] })),
            )
            .await,
        )
        .await;
        let role_id = created["id"].as_str().expect("created role id").to_string();

        let out = handle_update_role(
            &ctx,
            &update_role_msg(&role_id),
            body_input(serde_json::json!({ "description": "reads everything" })),
        )
        .await;
        assert!(out.collect_buffered().await.is_ok(), "update must succeed");

        let rows = db_read::list_every(
            &ctx,
            super::super::logs::AUDIT_LOGS_TABLE,
            vec![Filter {
                field: "action".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!("role.update"),
            }],
        )
        .await
        .expect("list audit rows");
        assert_eq!(rows.len(), 1, "a role update must leave an audit trail");
    }

    #[tokio::test]
    async fn update_role_rejects_mutation_when_guard_read_errors() {
        // Real system role exists in the DB (renaming it would break auth).
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let role_id = seed_system_role(&ctx).await;
        let failing = FailingGetContext { inner: ctx };

        // Attempt to rename the system role while the protective guard read
        // (db::get) is failing. The mutation must be rejected — not silently
        // let through because the guard couldn't be evaluated.
        let out = handle_update_role(
            &failing,
            &update_role_msg(&role_id),
            body_input(serde_json::json!({"name": "renamed-admin"})),
        )
        .await;
        assert!(
            output_is_error(out, "Internal").await,
            "a guard-read infra error must reject the mutation (fail closed)"
        );

        // Verify no rename actually happened — `list` isn't intercepted, so
        // this reads the real row through the same context.
        let records = db_read::list_every(
            &failing,
            ROLES_TABLE,
            vec![Filter {
                field: "id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!(role_id),
            }],
        )
        .await
        .expect("list role after failed update");
        assert_eq!(
            records[0].str_field("name"),
            "admin",
            "system role name must be unchanged after a fail-closed rejection"
        );
    }

    #[tokio::test]
    async fn update_role_still_forbids_system_role_rename_on_success() {
        // Regression guard: the normal (non-erroring) guard-read path must
        // still block a rename of a real system role.
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let role_id = seed_system_role(&ctx).await;

        let out = handle_update_role(
            &ctx,
            &update_role_msg(&role_id),
            body_input(serde_json::json!({"name": "renamed-admin"})),
        )
        .await;
        assert!(output_is_error(out, "PermissionDenied").await);
    }

    #[tokio::test]
    async fn update_role_missing_row_returns_not_found() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let out = handle_update_role(
            &ctx,
            &update_role_msg("does-not-exist"),
            body_input(serde_json::json!({"description": "x"})),
        )
        .await;
        assert!(output_is_error(out, "NotFound").await);
    }

    #[tokio::test]
    async fn update_role_non_system_role_updates_normally() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let data = json_map(serde_json::json!({
            "name": "editor",
            "description": "old",
            "is_system": false,
            "permissions": []
        }));
        let created = db::create(&ctx, ROLES_TABLE, data).await.unwrap();

        let out = handle_update_role(
            &ctx,
            &update_role_msg(&created.id),
            body_input(serde_json::json!({"name": "renamed-editor"})),
        )
        .await;
        let json = output_json(out).await;
        assert_eq!(json["name"], "renamed-editor");
        assert!(
            json.get("data").is_none(),
            "the record envelope must not survive the projection: {json}"
        );
    }

    /// Every field name a role write publishes, sorted — must equal what the
    /// list publishes, since all three go through `AdminRoleView`.
    fn role_fields(role: &serde_json::Value) -> Vec<&str> {
        let mut got: Vec<&str> = role
            .as_object()
            .expect("role object")
            .keys()
            .map(String::as_str)
            .collect();
        got.sort_unstable();
        got
    }

    const ROLE_VIEW_FIELDS: [&str; 7] = [
        "created_at",
        "description",
        "id",
        "is_system",
        "name",
        "permissions",
        "updated_at",
    ];

    /// `POST` used to `ok_json` the raw `db::Record` from `ops::create_role`
    /// — the `{id, data: {…}}` envelope with `permissions` in whatever
    /// encoding the backend returned. It must publish the list's projection.
    #[tokio::test]
    async fn create_role_publishes_the_list_projection() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let out = handle_create_role(
            &ctx,
            &admin_msg("create", "/b/admin/api/iam/roles"),
            body_input(serde_json::json!({
                "name": "editor",
                "description": "Can edit content",
                "permissions": ["posts.write"]
            })),
        )
        .await;
        let role = output_json(out).await;

        assert_eq!(role_fields(&role), ROLE_VIEW_FIELDS);
        assert_eq!(role["name"], serde_json::json!("editor"));
        assert_eq!(role["permissions"], serde_json::json!(["posts.write"]));
        assert_eq!(role["is_system"], serde_json::json!(false));
    }

    /// `POST /b/admin/api/iam/permissions` with a name that is already stored
    /// answers **409**, not a 500.
    ///
    /// `permissions.name` is `TEXT NOT NULL UNIQUE` with a matching unique
    /// index, so the third instance of the shape `ops::create_variable` and
    /// `ops::create_role` carried lived here: the same
    /// `err_internal("Database error", e)` tail, the same 500 for a request
    /// that named a permission which already exists.
    #[tokio::test]
    async fn creating_a_permission_whose_name_is_taken_is_a_conflict() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let permission =
            serde_json::json!({"name": "posts.write", "resource": "posts", "actions": ["write"]});
        let msg = routed(admin_msg("create", "/b/admin/api/iam/permissions"));
        output_json(handle_create_permission(&ctx, &msg, body_input(permission.clone())).await)
            .await;

        let out = handle_create_permission(&ctx, &msg, body_input(permission)).await;
        assert_eq!(
            crate::test_support::output_http_status(out).await,
            409,
            "a taken permission name is a conflict, not an internal error",
        );
    }

    /// `PATCH` publishes the same projection, and a `permissions` value the
    /// schema does not admit is refused rather than written.
    #[tokio::test]
    async fn update_role_publishes_the_list_projection_and_types_permissions() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let created = output_json(
            handle_create_role(
                &ctx,
                &admin_msg("create", "/b/admin/api/iam/roles"),
                body_input(serde_json::json!({"name": "editor"})),
            )
            .await,
        )
        .await;
        let role_id = created["id"].as_str().unwrap();

        let updated = output_json(
            handle_update_role(
                &ctx,
                &update_role_msg(role_id),
                body_input(serde_json::json!({"permissions": ["posts.read", "posts.write"]})),
            )
            .await,
        )
        .await;
        assert_eq!(role_fields(&updated), ROLE_VIEW_FIELDS);
        assert_eq!(
            updated["permissions"],
            serde_json::json!(["posts.read", "posts.write"])
        );

        let out = handle_update_role(
            &ctx,
            &update_role_msg(role_id),
            body_input(serde_json::json!({"permissions": "posts.*"})),
        )
        .await;
        assert!(
            output_is_error(out, "InvalidArgument").await,
            "a permissions value that is not an array of strings must be refused"
        );
    }

    #[tokio::test]
    async fn delete_role_reports_deleted() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let created = output_json(
            handle_create_role(
                &ctx,
                &admin_msg("create", "/b/admin/api/iam/roles"),
                body_input(serde_json::json!({"name": "editor"})),
            )
            .await,
        )
        .await;
        let msg = routed(admin_msg(
            "delete",
            &format!("/b/admin/api/iam/roles/{}", created["id"].as_str().unwrap()),
        ));

        let body = output_json(handle_delete_role(&ctx, &msg).await).await;
        assert_eq!(body, serde_json::json!({"deleted": true}));
    }

    /// P2c: assigning a role is a security-relevant grant — it must bump the
    /// target user's auth_version so an already-issued access JWT (minted
    /// with the old, smaller role set) is invalidated instead of keeping its
    /// stale `roles` claim until natural expiry.
    #[tokio::test]
    async fn assign_role_bumps_the_targets_auth_version() {
        use crate::blocks::auth::repo::users;

        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let uid = users::insert(
            &ctx,
            users::NewUser {
                email: "grantee@example.com".into(),
                display_name: "Grantee".into(),
                avatar_url: None,
                role: "user".into(),
                email_verified: false,
                verification_token_hash: None,
            },
        )
        .await
        .unwrap()
        .id;
        assert_eq!(users::auth_version(&ctx, &uid).await.unwrap(), 0);
        define_role(&ctx, "editor").await;

        let msg = admin_msg("create", "/b/admin/api/iam/user-roles");
        let out = handle_assign_role(
            &ctx,
            &msg,
            body_input(serde_json::json!({"user_id": uid, "role": "editor"})),
        )
        .await;
        assert!(
            !output_is_error(out, "Internal").await,
            "assign must succeed"
        );

        assert_eq!(
            users::auth_version(&ctx, &uid).await.unwrap(),
            1,
            "assigning a role must bump the target user's auth_version"
        );
    }

    /// P2c: removing a role (demotion) is exactly the change auth_version
    /// exists to invalidate — an already-issued JWT minted with the removed
    /// role must stop working immediately, not at its natural expiry.
    #[tokio::test]
    async fn remove_role_bumps_the_targets_auth_version() {
        use crate::blocks::auth::repo::users;

        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let uid = users::insert(
            &ctx,
            users::NewUser {
                email: "demotee@example.com".into(),
                display_name: "Demotee".into(),
                avatar_url: None,
                role: "user".into(),
                email_verified: false,
                verification_token_hash: None,
            },
        )
        .await
        .unwrap()
        .id;
        define_role(&ctx, "editor").await;

        let msg = admin_msg("create", "/b/admin/api/iam/user-roles");
        let assigned = output_json(
            handle_assign_role(
                &ctx,
                &msg,
                body_input(serde_json::json!({"user_id": uid, "role": "editor"})),
            )
            .await,
        )
        .await;
        let role_row_id = assigned["id"]
            .as_str()
            .expect("assign response carries the user_roles row id")
            .to_string();
        // The assign above already bumped once; capture that baseline so the
        // removal's OWN bumps are what this test proves.
        let before_remove = users::auth_version(&ctx, &uid).await.unwrap();

        let remove_msg = routed(admin_msg(
            "delete",
            &format!("/b/admin/api/iam/user-roles/{role_row_id}"),
        ));
        let out = handle_remove_role(&ctx, &remove_msg).await;
        assert!(
            !output_is_error(out, "Internal").await,
            "remove must succeed"
        );

        assert_eq!(
            users::auth_version(&ctx, &uid).await.unwrap(),
            before_remove + 2,
            "removing a role bumps the target user's auth_version before the \
             removal and again after it"
        );
    }

    /// A rename onto a role name a user already holds a grant of leaves that
    /// user with one grant, not a rewrite the unique index refuses.
    ///
    /// The assign endpoint now grants only defined roles and a role delete
    /// revokes its grants, so neither makes such a grant any more. A database
    /// can still hold one: a grant of a role deleted before this release
    /// outlived it, and `RELEASE.md` tells operators how to find them. That
    /// grant is planted here the way it was left — through the table's
    /// writer, with no role behind it — and the `editor` grant is made the
    /// way an admin makes one today.
    #[tokio::test]
    async fn a_rename_onto_a_name_already_held_merges_the_grants() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.seed_auth_user("holder").await;
        let role_id = define_role(&ctx, "editor").await;
        output_json(
            handle_assign_role(
                &ctx,
                &admin_msg("create", "/b/admin/api/iam/user-roles"),
                body_input(serde_json::json!({"user_id": "holder", "role": "editor"})),
            )
            .await,
        )
        .await;
        assert!(matches!(
            user_roles::assign(&ctx, "holder", "editor-v2", "").await,
            Ok(Assigned::Created(_))
        ));

        let out = handle_update_role(
            &ctx,
            &update_role_msg(&role_id),
            body_input(serde_json::json!({"name": "editor-v2"})),
        )
        .await;
        assert_eq!(output_json(out).await["name"], "editor-v2");

        let names: Vec<String> = user_roles::list_for_user(&ctx, "holder")
            .await
            .expect("list grants")
            .into_iter()
            .map(|row| row.role)
            .collect();
        assert_eq!(names, vec!["editor-v2"]);
    }

    /// Deleting a role revokes it: the next token its former holder is minted
    /// no longer names it, and a role created again under the same name does
    /// not quietly hand it back.
    ///
    /// The token is minted by the real login handler, AFTER the delete — the
    /// `roles` claim is built from the grant rows alone, without consulting
    /// the role definitions, so a grant the delete left behind shows up
    /// exactly there.
    #[tokio::test]
    async fn deleting_a_role_revokes_it_from_every_token_minted_afterwards() {
        use crate::blocks::{
            auth::repo::users,
            auth_ui::api::{login, signup, test_mail_request},
        };

        // The sign-up and logins run as auth-ui and the role edits as the
        // admin block, each in its own frame; `ctx` stages and asserts.
        let ctx = TestContext::with_auth_and_crypto().await.fixture();
        let ui = ctx
            .clone()
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID);
        let admin = ctx.clone().running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let creds = serde_json::json!({
            "email": "grantee@example.com",
            "password": "correct-horse-battery",
        });
        let (limiter, msg) = test_mail_request();
        output_json(signup::handle(&limiter, &ui, &msg, body_input(creds.clone())).await).await;
        let uid = users::find_by_email(&ctx, "grantee@example.com")
            .await
            .expect("user lookup")
            .expect("signup created the user")
            .id;

        let role_id = define_role(&admin, "editor").await;
        output_json(
            handle_assign_role(
                &admin,
                &admin_msg("create", "/b/admin/api/iam/user-roles"),
                body_input(serde_json::json!({"user_id": uid, "role": "editor"})),
            )
            .await,
        )
        .await;

        let roles_in_a_fresh_token = |ctx: TestContext| {
            let creds = creds.clone();
            async move {
                let token = output_json(login::handle(&ctx, body_input(creds)).await).await
                    ["access_token"]
                    .as_str()
                    .expect("login mints an access token")
                    .to_string();
                // The claims as minted: the payload segment, decoded. What
                // is asserted is what the token says, not whether this
                // fixture's signing key is the one verification derives.
                use base64ct::Encoding;
                let payload = token.split('.').nth(1).expect("a JWT has a payload");
                let claims: serde_json::Value = serde_json::from_slice(
                    &base64ct::Base64UrlUnpadded::decode_vec(payload).expect("base64url payload"),
                )
                .expect("JSON claims");
                claims["roles"]
                    .as_array()
                    .expect("the token carries a roles claim")
                    .iter()
                    .filter_map(|r| r.as_str().map(str::to_string))
                    .collect::<Vec<String>>()
            }
        };
        assert!(
            roles_in_a_fresh_token(ui.clone())
                .await
                .contains(&"editor".to_string()),
            "precondition: the grant reaches the token"
        );

        let before = users::auth_version(&ctx, &uid).await.expect("auth version");
        let out = handle_delete_role(
            &admin,
            &routed(admin_msg(
                "delete",
                &format!("/b/admin/api/iam/roles/{role_id}"),
            )),
        )
        .await;
        assert_eq!(output_json(out).await, serde_json::json!({"deleted": true}));

        let after_delete = roles_in_a_fresh_token(ui.clone()).await;
        assert!(
            !after_delete.contains(&"editor".to_string()),
            "a token minted after the delete must not carry the role: {after_delete:?}"
        );
        assert!(
            users::auth_version(&ctx, &uid).await.expect("auth version") > before,
            "tokens minted while the role existed must stop authenticating"
        );

        let audit = db_read::list_every(
            &ctx,
            super::super::logs::AUDIT_LOGS_TABLE,
            vec![Filter {
                field: "action".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!("role.delete"),
            }],
        )
        .await
        .expect("list audit rows");
        assert_eq!(
            audit[0].str_field("resource"),
            format!("roles/{role_id} (name: editor; grants revoked: 1)"),
            "the audit row says which role went and how many grants went with it"
        );

        define_role(&admin, "editor").await;
        let after_recreate = roles_in_a_fresh_token(ui.clone()).await;
        assert!(
            !after_recreate.contains(&"editor".to_string()),
            "a new role under the old name must not re-attach the old grant: {after_recreate:?}"
        );
    }

    /// A delete of a role that is not there is a 404 from the guard read,
    /// and touches no grant — not even one naming the id it was given.
    ///
    /// Passes on the code before the revocation existed, by design: that
    /// code revoked nothing at all. It pins that the revocation added since
    /// runs only for a role that was found, and never keys on the path id.
    #[tokio::test]
    async fn deleting_a_missing_role_is_not_found_and_revokes_nothing() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        // A grant of a name no role has, as a role deleted before this
        // release left behind.
        user_roles::assign(&ctx, "u-1", "ghost", "")
            .await
            .expect("plant the grant");

        let out = handle_delete_role(
            &ctx,
            &routed(admin_msg("delete", "/b/admin/api/iam/roles/ghost")),
        )
        .await;
        assert!(output_is_error(out, "NotFound").await);
        assert_eq!(
            user_roles::list_by_role(&ctx, "ghost")
                .await
                .expect("list")
                .len(),
            1,
            "a refused delete revokes nothing"
        );
    }

    /// The assign endpoint grants only a role that is defined.
    ///
    /// A grant of an undefined name reaches the holder's token regardless —
    /// the `roles` claim is built from the grant rows alone — and a role
    /// later created under that name re-attaches to it.
    #[tokio::test]
    async fn assigning_an_undefined_role_is_refused() {
        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.seed_auth_user("u-1").await;
        let out = handle_assign_role(
            &ctx,
            &admin_msg("create", "/b/admin/api/iam/user-roles"),
            body_input(serde_json::json!({"user_id": "u-1", "role": "editor"})),
        )
        .await;
        assert!(output_is_error(out, "InvalidArgument").await);
        assert!(user_roles::list_for_user(&ctx, "u-1")
            .await
            .expect("list")
            .is_empty());
    }

    /// Two concurrent assigns of one role to one user leave ONE grant — and
    /// so the revoke endpoint, which removes one row, really revokes.
    ///
    /// `assign` reads before it inserts, and two callers that both pass the
    /// read see no grant. The rendezvous holds both on that read (a
    /// `database.list` of the grants table; the handler's role-definition
    /// lookup is on another table and passes through) until each has made
    /// it — the interleaving two concurrent bootstrap-admin logins produce
    /// through `TokenGrant::resolve`, and two admins clicking "assign" at once
    /// through this handler. Without the unique index both inserts land, and
    /// `handle_remove_role` then deletes one of them, answers
    /// `{"deleted": true}`, and the twin keeps the role live.
    ///
    /// Names `user_roles::TABLE` only to aim the rendezvous; `tests/repo_door.rs`
    /// allowlists it as a fault injector.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn racing_assigns_leave_one_grant_that_the_revoke_endpoint_removes() {
        use crate::test_support::RendezvousDbOpContext;

        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.seed_auth_user("racer").await;
        define_role(&ctx, "auditor").await;
        let gated = RendezvousDbOpContext::new(ctx.clone(), "database.list", user_roles::TABLE, 2);

        let racers: Vec<_> = (0..2)
            .map(|_| {
                let gated = gated.clone();
                tokio::spawn(async move {
                    crate::test_support::output_http_status(
                        handle_assign_role(
                            &gated,
                            &admin_msg("create", "/b/admin/api/iam/user-roles"),
                            body_input(serde_json::json!({"user_id": "racer", "role": "auditor"})),
                        )
                        .await,
                    )
                    .await
                })
            })
            .collect();
        let mut statuses = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            futures::future::try_join_all(racers),
        )
        .await
        .expect("both assigns must reach the rendezvous and finish")
        .expect("assign task panicked");
        statuses.sort_unstable();
        assert_eq!(
            statuses,
            vec![200, 409],
            "one assign writes the grant, the other is told it is already held"
        );

        let rows = user_roles::list_for_user(&ctx, "racer")
            .await
            .expect("list grants");
        assert_eq!(rows.len(), 1, "one grant, not one per racer: {rows:?}");

        let out = handle_remove_role(
            &ctx,
            &routed(admin_msg(
                "delete",
                &format!("/b/admin/api/iam/user-roles/{}", rows[0].id),
            )),
        )
        .await;
        assert_eq!(output_json(out).await, serde_json::json!({"deleted": true}));
        let roles = crate::blocks::auth::helpers::get_user_roles(&ctx, "racer")
            .await
            .expect("resolve roles");
        assert!(
            !roles.iter().any(|r| r == "auditor"),
            "a revoke that reports success must leave the role gone: {roles:?}"
        );
    }

    /// A role delete whose session invalidation fails leaves every grant in
    /// place, so deleting again finishes the job — and bumps every holder.
    ///
    /// The shape this replaces removed a grant and THEN bumped its holder; a
    /// failed bump left that grant gone, so a retry's read never found the
    /// holder again, and their live token kept the deleted role until it
    /// expired. The injector fails every auth-version bump, which is the
    /// first write the delete makes.
    ///
    /// Names `users::TABLE` only to aim the fault injector;
    /// `tests/repo_door.rs` allowlists it as one.
    #[tokio::test]
    async fn a_role_delete_whose_invalidation_fails_can_be_retried() {
        use crate::{blocks::auth::repo::users, test_support::FailingDbOpContext};

        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let role_id = define_role(&ctx, "editor").await;
        for user in ["u-1", "u-2"] {
            ctx.seed_auth_user(user).await;
            output_json(
                handle_assign_role(
                    &ctx,
                    &admin_msg("create", "/b/admin/api/iam/user-roles"),
                    body_input(serde_json::json!({"user_id": user, "role": "editor"})),
                )
                .await,
            )
            .await;
        }
        let delete = || {
            routed(admin_msg(
                "delete",
                &format!("/b/admin/api/iam/roles/{role_id}"),
            ))
        };

        let failing = FailingDbOpContext::new(
            ctx.clone(),
            vec![("database.increment_field_where", users::TABLE)],
        );
        assert!(output_is_error(handle_delete_role(&failing, &delete()).await, "Internal").await);
        assert_eq!(
            user_roles::list_by_role(&ctx, "editor")
                .await
                .expect("list")
                .len(),
            2,
            "a delete that could not invalidate sessions must not have revoked a grant"
        );

        let before: Vec<i64> =
            futures::future::try_join_all(["u-1", "u-2"].map(|u| users::auth_version(&ctx, u)))
                .await
                .expect("auth versions");
        assert_eq!(
            output_json(handle_delete_role(&ctx, &delete()).await).await,
            serde_json::json!({"deleted": true})
        );
        assert!(user_roles::list_by_role(&ctx, "editor")
            .await
            .expect("list")
            .is_empty());
        for (user, was) in ["u-1", "u-2"].into_iter().zip(before) {
            assert!(
                users::auth_version(&ctx, user).await.expect("auth version") > was,
                "the retry bumps {user}"
            );
        }
    }

    /// A rename onto a name another role holds is a **409**, and moves
    /// nothing: no grant is rewritten and no holder's sessions are
    /// invalidated.
    ///
    /// `roles.name` is UNIQUE, so the role update refuses it — as the create
    /// path's insert refuses the same name, which `ops::create_role` already
    /// answers 409. The update answered 500. The grant and auth-version
    /// assertions are what a status check alone would not pin: they hold only
    /// because the role row is written before the cascade, and fail on any
    /// order that moves a grant first.
    #[tokio::test]
    async fn a_rename_onto_another_roles_name_is_a_conflict_that_moves_nothing() {
        use crate::blocks::auth::repo::users;

        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let editor_id = define_role(&ctx, "editor").await;
        define_role(&ctx, "author").await;
        for (user, role) in [("u-1", "editor"), ("u-2", "author")] {
            ctx.seed_auth_user(user).await;
            output_json(
                handle_assign_role(
                    &ctx,
                    &admin_msg("create", "/b/admin/api/iam/user-roles"),
                    body_input(serde_json::json!({"user_id": user, "role": role})),
                )
                .await,
            )
            .await;
        }
        let versions =
            || futures::future::try_join_all(["u-1", "u-2"].map(|u| users::auth_version(&ctx, u)));
        let before = versions().await.expect("auth versions");

        let out = handle_update_role(
            &ctx,
            &update_role_msg(&editor_id),
            body_input(serde_json::json!({"name": "author"})),
        )
        .await;
        assert_eq!(
            crate::test_support::output_http_status(out).await,
            409,
            "a rename onto a taken role name is a conflict, not an internal error"
        );

        assert_eq!(
            db::get(&ctx, ROLES_TABLE, &editor_id)
                .await
                .expect("role row")
                .str_field("name"),
            "editor"
        );
        for (role, holder) in [("editor", "u-1"), ("author", "u-2")] {
            let holders: Vec<String> = user_roles::list_by_role(&ctx, role)
                .await
                .expect("list grants")
                .into_iter()
                .map(|g| g.user_id)
                .collect();
            assert_eq!(holders, vec![holder], "the {role} grants must be untouched");
        }
        assert_eq!(
            versions().await.expect("auth versions"),
            before,
            "a refused rename must not invalidate anyone's sessions"
        );
    }

    /// A role delete whose revocation pass AFTER the row delete fails still
    /// reports the deletion, and still leaves its audit row.
    ///
    /// By then the role is gone. Answering with an error said it was not —
    /// and skipped the `role.delete` audit row for a deletion that happened.
    /// The injector lets the first pass's grants read through and fails the
    /// second's, which is the only read of that table the delete makes after
    /// the role row is deleted.
    ///
    /// Names `user_roles::TABLE` only to aim the fault injector;
    /// `tests/repo_door.rs` allowlists it as one.
    #[tokio::test]
    async fn a_role_delete_whose_late_revocation_pass_fails_still_reports_the_deletion() {
        use crate::test_support::FailingDbOpContext;

        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let role_id = define_role(&ctx, "editor").await;
        ctx.seed_auth_user("u-1").await;
        output_json(
            handle_assign_role(
                &ctx,
                &admin_msg("create", "/b/admin/api/iam/user-roles"),
                body_input(serde_json::json!({"user_id": "u-1", "role": "editor"})),
            )
            .await,
        )
        .await;

        let failing =
            FailingDbOpContext::new(ctx.clone(), vec![("database.list", user_roles::TABLE)])
                .after_passing(1);
        let out = handle_delete_role(
            &failing,
            &routed(admin_msg(
                "delete",
                &format!("/b/admin/api/iam/roles/{role_id}"),
            )),
        )
        .await;
        assert_eq!(
            output_json(out).await,
            serde_json::json!({"deleted": true}),
            "the role is gone, so the response must say so"
        );
        assert!(
            matches!(db::get(&ctx, ROLES_TABLE, &role_id).await, Err(e) if e.code == ErrorCode::NotFound),
            "precondition: the role row really was deleted"
        );

        let audit = db_read::list_every(
            &ctx,
            super::super::logs::AUDIT_LOGS_TABLE,
            vec![Filter {
                field: "action".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!("role.delete"),
            }],
        )
        .await
        .expect("list audit rows");
        assert_eq!(audit.len(), 1, "a deletion that happened is audited");
        assert_eq!(
            audit[0].str_field("resource"),
            format!(
                "roles/{role_id} (name: editor; grants revoked: 1; the revocation pass after \
                 the delete failed while it was reading the grants)"
            ),
            "the audit row records the failed pass"
        );
    }

    /// A rename whose grant cascade stops part-way has still renamed the
    /// role: the row carries the new name before the first grant moves. It
    /// was answered 500 with no `role.update` audit row — a mutation that
    /// happened, reported as one that did not, and left unaudited. It is a
    /// 200 carrying the renamed role and a `warning` saying which grants are
    /// left, and the audit row records where the cascade stopped.
    ///
    /// The injector lets the first grant's rewrite through and fails the
    /// second's, so the cascade genuinely stops between two grants.
    ///
    /// Names `user_roles::TABLE` only to aim the fault injector;
    /// `tests/repo_door.rs` allowlists it as one.
    #[tokio::test]
    async fn a_rename_whose_cascade_stops_part_way_is_audited_and_answered_as_a_rename() {
        use crate::test_support::FailingDbOpContext;

        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let role_id = define_role(&ctx, "editor").await;
        for user in ["u-1", "u-2"] {
            ctx.seed_auth_user(user).await;
            output_json(
                handle_assign_role(
                    &ctx,
                    &admin_msg("create", "/b/admin/api/iam/user-roles"),
                    body_input(serde_json::json!({"user_id": user, "role": "editor"})),
                )
                .await,
            )
            .await;
        }

        let failing =
            FailingDbOpContext::new(ctx.clone(), vec![("database.update", user_roles::TABLE)])
                .after_passing(1);
        let out = handle_update_role(
            &failing,
            &update_role_msg(&role_id),
            body_input(serde_json::json!({"name": "author"})),
        )
        .await;
        let parts = wafer_block::http_codec::collect_http_response(out).await;
        let body: serde_json::Value =
            serde_json::from_slice(&parts.body).expect("the answer is a JSON body");
        assert_eq!(
            parts.status, 200,
            "the rename happened, so it must not be answered as a failure: {body}"
        );
        assert_eq!(
            body["name"], "author",
            "the answer is the renamed role: {body}"
        );
        assert_eq!(
            body["warning"],
            "Role renamed to \"author\", but only 1 of its 2 grants moved. 1 still name \
             \"editor\", which no role is called any more, so their holders do not hold this \
             role. Rename the role back to \"editor\" and then to \"author\" again to move \
             the rest."
        );

        assert_eq!(
            db::get(&ctx, ROLES_TABLE, &role_id)
                .await
                .expect("role row")
                .str_field("name"),
            "author",
            "precondition: the role row really was renamed"
        );
        for role in ["author", "editor"] {
            assert_eq!(
                user_roles::list_by_role(&ctx, role)
                    .await
                    .expect("list grants")
                    .len(),
                1,
                "precondition: the cascade really stopped between the two grants ({role})"
            );
        }

        let audit = db_read::list_every(
            &ctx,
            super::super::logs::AUDIT_LOGS_TABLE,
            vec![Filter {
                field: "action".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!("role.update"),
            }],
        )
        .await
        .expect("list audit rows");
        assert_eq!(audit.len(), 1, "a rename that happened is audited");
        assert_eq!(
            audit[0].str_field("resource"),
            format!(
                "roles/{role_id} (renamed from editor to author; the grant cascade stopped after \
                 moving 1 of 2 grants)"
            ),
            "the audit row records how far the cascade got"
        );
    }

    /// `u-1` granted before the delete and `late` granted while it is in
    /// flight, and a role delete driven over `wrap` — the context that lands
    /// the `late` grants between the two revocation passes.
    async fn delete_with_an_in_flight_assign(
        late: &'static [&'static str],
        wrap: impl FnOnce(TestContext) -> crate::test_support::FailingDbOpContext,
    ) -> (TestContext, String, super::super::ops::RoleDeleted) {
        use super::super::test_support::AssignBeforeGrantRead;

        let ctx = TestContext::with_auth()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let role_id = define_role(&ctx, "editor").await;
        for user in std::iter::once(&"u-1").chain(late) {
            ctx.seed_auth_user(user).await;
        }
        output_json(
            handle_assign_role(
                &ctx,
                &admin_msg("create", "/b/admin/api/iam/user-roles"),
                body_input(serde_json::json!({"user_id": "u-1", "role": "editor"})),
            )
            .await,
        )
        .await;

        let racing = AssignBeforeGrantRead::new(ctx.clone(), wrap(ctx.clone()), 2, late, "editor");
        let deleted = super::super::ops::delete_role(
            &racing,
            &routed(admin_msg(
                "delete",
                &format!("/b/admin/api/iam/roles/{role_id}"),
            )),
            &role_id,
        )
        .await
        .unwrap_or_else(|_| panic!("the role row is deleted, so the delete is not an error"));
        (ctx, role_id, deleted)
    }

    /// A grant assigned while a role delete is in flight, which the pass after
    /// the role row is gone reads but cannot revoke, outlives the role — and
    /// creating a role of the same name would hand it back. The delete names
    /// its holder: in the outcome the roles tab's toast is built from, and on
    /// the audit row. It named no one, so the operator was told to "check its
    /// former holders" with no way to know who they were.
    ///
    /// The injector fails the second revoke statement, which is the late
    /// pass's; the first pass's goes through.
    ///
    /// Names `user_roles::TABLE` only to aim the fault injector;
    /// `tests/repo_door.rs` allowlists it as one.
    #[tokio::test]
    async fn a_late_grant_the_delete_could_not_revoke_is_reported_with_its_holder() {
        use crate::test_support::FailingDbOpContext;

        let (ctx, role_id, deleted) = delete_with_an_in_flight_assign(&["u-late"], |ctx| {
            FailingDbOpContext::new(
                ctx,
                vec![("database.delete_where_count", user_roles::TABLE)],
            )
            .after_passing(1)
        })
        .await;

        assert_eq!(
            deleted,
            super::super::ops::RoleDeleted::LateGrantsNotRevoked {
                holders: Some(vec!["u-late".to_string()]),
            }
        );
        let left: Vec<String> = user_roles::list_by_role(&ctx, "editor")
            .await
            .expect("list grants")
            .into_iter()
            .map(|g| g.user_id)
            .collect();
        assert_eq!(
            left,
            vec!["u-late"],
            "precondition: the in-flight grant really outlived the role"
        );

        let audit = db_read::list_every(
            &ctx,
            super::super::logs::AUDIT_LOGS_TABLE,
            vec![Filter {
                field: "action".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::json!("role.delete"),
            }],
        )
        .await
        .expect("list audit rows");
        assert_eq!(audit.len(), 1, "a deletion that happened is audited");
        assert_eq!(
            audit[0].str_field("resource"),
            format!(
                "roles/{role_id} (name: editor; grants revoked: 1; the revocation pass after \
                 the delete failed while it was revoking the grants; holders it found: u-late)"
            ),
            "the audit row names the holder of the grant that may remain"
        );
    }

    /// When the late pass revokes the in-flight grants and then fails to
    /// invalidate a holder's sessions, no grant is left: the outcome says so,
    /// rather than warning about a grant that is gone, and names only the
    /// holders whose sessions were not invalidated. The pass stops at the
    /// first failure, so the holder before it WAS invalidated and is not
    /// named.
    ///
    /// Bumps run before and after each pass's revoke. The first pass bumps
    /// `u-1` twice; the late pass bumps `u-late-a` and `u-late-b` before its
    /// revoke, then `u-late-a` after it — five that pass. The sixth,
    /// `u-late-b`'s after the revoke, fails.
    ///
    /// Names `users::TABLE` only to aim the fault injector;
    /// `tests/repo_door.rs` allowlists it as one.
    #[tokio::test]
    async fn a_late_pass_that_revoked_but_could_not_invalidate_names_the_sessions_left() {
        use crate::{blocks::auth::repo::users, test_support::FailingDbOpContext};

        let (ctx, _, deleted) = delete_with_an_in_flight_assign(&["u-late-a", "u-late-b"], |ctx| {
            FailingDbOpContext::new(ctx, vec![("database.increment_field_where", users::TABLE)])
                .after_passing(5)
        })
        .await;

        assert_eq!(
            deleted,
            super::super::ops::RoleDeleted::LateSessionsNotInvalidated {
                holders: vec!["u-late-b".to_string()],
            }
        );
        assert!(
            user_roles::list_by_role(&ctx, "editor")
                .await
                .expect("list grants")
                .is_empty(),
            "precondition: the late pass really revoked the in-flight grant"
        );
    }

    /// Dispatch one admin JSON request through the block's own `handle` —
    /// the route table binds `{id}`, and `handle_create_permission` gets the
    /// `Message` the route hands it rather than one a test built.
    async fn permissions_api(
        ctx: &TestContext,
        action: &str,
        path: &str,
        body: &str,
    ) -> OutputStream {
        wafer_run::Block::handle(
            &crate::blocks::admin::AdminBlock::new(),
            ctx,
            routed(admin_msg(action, path)),
            InputStream::from_bytes(body.as_bytes().to_vec()),
        )
        .await
    }

    /// The permission catalogue is what every role is written against, so
    /// adding an entry has to leave the same trail a role does. It wrote
    /// none: the route handed `handle_create_permission` no `Message`, so it
    /// had no admin to attribute the change to.
    #[tokio::test]
    async fn creating_a_permission_writes_an_audit_row_naming_it() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);

        let created = output_json(
            permissions_api(
                &ctx,
                "create",
                "/b/admin/api/iam/permissions",
                r#"{"name":"posts.publish","resource":"posts","actions":["publish"]}"#,
            )
            .await,
        )
        .await;
        let id = created["id"].as_str().expect("the created row's id");

        let rows = audit_rows(&ctx, "permission.create").await;
        assert_eq!(rows.len(), 1, "one row per created permission");
        assert_eq!(rows[0].str_field("resource"), format!("permissions/{id}"));
        assert_eq!(
            rows[0].str_field("user_id"),
            "admin_1",
            "attributed to the admin the route authenticated"
        );
    }

    /// Deleting a permission silently un-grants whatever referenced it, so
    /// it is the half of the pair that most needs a row. It returned
    /// `{"deleted": true}` and wrote nothing.
    #[tokio::test]
    async fn deleting_a_permission_writes_an_audit_row_naming_it() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let created = output_json(
            permissions_api(
                &ctx,
                "create",
                "/b/admin/api/iam/permissions",
                r#"{"name":"posts.publish","resource":"posts","actions":["publish"]}"#,
            )
            .await,
        )
        .await;
        let id = created["id"]
            .as_str()
            .expect("the created row's id")
            .to_string();

        let deleted = output_json(
            permissions_api(
                &ctx,
                "delete",
                &format!("/b/admin/api/iam/permissions/{id}"),
                "",
            )
            .await,
        )
        .await;
        assert_eq!(deleted["deleted"], serde_json::json!(true));

        let rows = audit_rows(&ctx, "permission.delete").await;
        assert_eq!(rows.len(), 1, "one row per deleted permission");
        assert_eq!(rows[0].str_field("resource"), format!("permissions/{id}"));
    }

    /// A delete that found no row changed nothing, so it must not claim in
    /// the trail that a permission was removed.
    #[tokio::test]
    async fn a_delete_that_matched_nothing_is_not_audited() {
        let ctx = TestContext::with_admin()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);

        let out = permissions_api(
            &ctx,
            "delete",
            "/b/admin/api/iam/permissions/no-such-permission",
            "",
        )
        .await;
        assert!(
            output_is_error(out, "NotFound").await,
            "a 404, not a success"
        );
        assert_eq!(audit_rows(&ctx, "permission.delete").await.len(), 0);
    }
}
