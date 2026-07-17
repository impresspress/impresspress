use std::collections::HashMap;

use wafer_block::db::{Filter, FilterOp, ListOptions, SortField};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, ErrorCode, InputStream, Message, OutputStream};

use super::logs::audit_log;
use crate::{
    blocks::auth::bump_auth_version,
    http::{err_bad_request, err_conflict, err_forbidden, err_internal, err_not_found, ok_json},
    util::{json_map, RecordExt},
};

/// Role definitions table (one row per named role).
pub(crate) const ROLES_TABLE: &str = "impresspress__admin__roles";

/// Per-role permission rows (resource + actions tuples).
pub(crate) const PERMISSIONS_TABLE: &str = "impresspress__admin__permissions";

/// User → role assignment table (many-to-many via row per pair).
pub(crate) const USER_ROLES_TABLE: &str = "impresspress__admin__user_roles";

/// `path` is the normalized `/admin/iam/...` sub-path, passed explicitly (no
/// `req.resource` rewrite). Id-bearing leaves take their id from it.
pub async fn handle(
    ctx: &dyn Context,
    msg: &Message,
    path: &str,
    input: InputStream,
) -> OutputStream {
    let action = msg.action();

    match (action, path) {
        // Roles
        ("retrieve", "/admin/iam/roles") => handle_list_roles(ctx).await,
        ("create", "/admin/iam/roles") => handle_create_role(ctx, msg, input).await,
        ("update", _) if path.starts_with("/admin/iam/roles/") => {
            handle_update_role(ctx, path, input).await
        }
        ("delete", _) if path.starts_with("/admin/iam/roles/") => {
            handle_delete_role(ctx, msg, path).await
        }
        // Permissions
        ("retrieve", "/admin/iam/permissions") => handle_list_permissions(ctx).await,
        ("create", "/admin/iam/permissions") => handle_create_permission(ctx, input).await,
        ("delete", _) if path.starts_with("/admin/iam/permissions/") => {
            handle_delete_permission(ctx, path).await
        }
        // User-role assignments
        ("retrieve", "/admin/iam/user-roles") => handle_list_user_roles(ctx, msg).await,
        ("create", "/admin/iam/user-roles") => handle_assign_role(ctx, msg, input).await,
        ("delete", _) if path.starts_with("/admin/iam/user-roles/") => {
            handle_remove_role(ctx, msg, path).await
        }
        _ => err_not_found("not found"),
    }
}

async fn handle_list_roles(ctx: &dyn Context) -> OutputStream {
    let opts = ListOptions {
        sort: vec![SortField {
            field: "name".to_string(),
            desc: false,
        }],
        limit: 1000,
        ..Default::default()
    };
    match db::list(ctx, ROLES_TABLE, &opts).await {
        Ok(result) => ok_json(&result),
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_create_role(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    #[derive(serde::Deserialize)]
    struct Req {
        name: String,
        description: Option<String>,
        permissions: Option<Vec<String>>,
    }
    let raw = input.collect_to_bytes().await;
    let body: Req = match serde_json::from_slice(&raw) {
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
        Ok(record) => ok_json(&record),
        Err(out) => out,
    }
}

async fn handle_update_role(ctx: &dyn Context, path: &str, input: InputStream) -> OutputStream {
    let id = path.strip_prefix("/admin/iam/roles/").unwrap_or("");
    if id.is_empty() {
        return err_bad_request("Missing role ID");
    }

    let raw = input.collect_to_bytes().await;
    let body_peek: HashMap<String, serde_json::Value> = match serde_json::from_slice(&raw) {
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
        Err(e) if e.code == ErrorCode::NotFound => return err_not_found("Role not found"),
        Err(e) => return err_internal("Database error", e),
    };

    if existing.bool_field("is_system") {
        if body_peek.contains_key("name") {
            return err_forbidden("Cannot rename system roles");
        }
        let mut data = HashMap::new();
        for key in &["description", "permissions"] {
            if let Some(val) = body_peek.get(*key) {
                data.insert(key.to_string(), val.clone());
            }
        }
        crate::util::stamp_updated(&mut data);
        return match db::update(ctx, ROLES_TABLE, id, data).await {
            Ok(record) => ok_json(&record),
            Err(e) => err_internal("Database error", e),
        };
    }

    let mut data = HashMap::new();
    for key in &["name", "description", "permissions"] {
        if let Some(val) = body_peek.get(*key) {
            data.insert(key.to_string(), val.clone());
        }
    }
    crate::util::stamp_updated(&mut data);
    match db::update(ctx, ROLES_TABLE, id, data).await {
        Ok(record) => ok_json(&record),
        Err(e) if e.code == ErrorCode::NotFound => err_not_found("Role not found"),
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_delete_role(ctx: &dyn Context, msg: &Message, path: &str) -> OutputStream {
    let id = path.strip_prefix("/admin/iam/roles/").unwrap_or("");
    // System-role guard, delete, and audit-log write live in the shared ops
    // layer (the JSON path previously logged nothing).
    match super::ops::delete_role(ctx, msg, id).await {
        Ok(()) => ok_json(&serde_json::json!({"deleted": true})),
        Err(out) => out,
    }
}

async fn handle_list_permissions(ctx: &dyn Context) -> OutputStream {
    match db::list_all(ctx, PERMISSIONS_TABLE, vec![]).await {
        Ok(records) => {
            let total_count = records.len() as i64;
            ok_json(&db::RecordList {
                records,
                total_count,
                page: 1,
                page_size: total_count,
            })
        }
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_create_permission(ctx: &dyn Context, input: InputStream) -> OutputStream {
    #[derive(serde::Deserialize)]
    struct Req {
        name: String,
        resource: String,
        actions: Vec<String>,
    }
    let raw = input.collect_to_bytes().await;
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
        Ok(record) => ok_json(&record),
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_delete_permission(ctx: &dyn Context, path: &str) -> OutputStream {
    let id = path.strip_prefix("/admin/iam/permissions/").unwrap_or("");
    if id.is_empty() {
        return err_bad_request("Missing permission ID");
    }
    match db::delete(ctx, PERMISSIONS_TABLE, id).await {
        Ok(()) => ok_json(&serde_json::json!({"deleted": true})),
        Err(e) if e.code == ErrorCode::NotFound => err_not_found("Permission not found"),
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_list_user_roles(ctx: &dyn Context, msg: &Message) -> OutputStream {
    let user_id = msg.query("user_id").to_string();
    let mut filters = Vec::new();
    if !user_id.is_empty() {
        filters.push(Filter {
            field: "user_id".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(user_id),
        });
    }
    match db::list_all(ctx, USER_ROLES_TABLE, filters).await {
        Ok(records) => {
            let total_count = records.len() as i64;
            ok_json(&db::RecordList {
                records,
                total_count,
                page: 1,
                page_size: total_count,
            })
        }
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_assign_role(ctx: &dyn Context, msg: &Message, input: InputStream) -> OutputStream {
    #[derive(serde::Deserialize)]
    struct Req {
        user_id: String,
        role: String,
    }
    let raw = input.collect_to_bytes().await;
    let body: Req = match serde_json::from_slice(&raw) {
        Ok(b) => b,
        Err(e) => return err_bad_request(&format!("Invalid body: {e}")),
    };

    // Check if already assigned
    let existing = db::list_all(
        ctx,
        USER_ROLES_TABLE,
        vec![
            Filter {
                field: "user_id".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::Value::String(body.user_id.clone()),
            },
            Filter {
                field: "role".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::Value::String(body.role.clone()),
            },
        ],
    )
    .await;
    match existing {
        Ok(records) => {
            if !records.is_empty() {
                return err_conflict("Role already assigned to user");
            }
        }
        Err(e) => return err_internal("Database error", e),
    }

    let assigned = format!("users/{}/roles/{}", body.user_id, body.role);
    let data = json_map(serde_json::json!({
        "user_id": body.user_id,
        "role": body.role,
        "assigned_at": crate::util::now_rfc3339(),
        "assigned_by": msg.user_id()
    }));
    match db::create(ctx, USER_ROLES_TABLE, data).await {
        Ok(record) => {
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
                return err_internal("Role assigned but session invalidation failed", e);
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
            ok_json(&record)
        }
        Err(e) => err_internal("Database error", e),
    }
}

async fn handle_remove_role(ctx: &dyn Context, msg: &Message, path: &str) -> OutputStream {
    let id = path.strip_prefix("/admin/iam/user-roles/").unwrap_or("");
    if id.is_empty() {
        return err_bad_request("Missing user-role ID");
    }

    // Prevent admins from removing their own admin role (self-lockout).
    // Also captures the affected user id so a successful removal can bump
    // their auth_version (P2c) below.
    let role_user = match db::get(ctx, USER_ROLES_TABLE, id).await {
        Ok(record) => {
            let role_user = record.str_field("user_id").to_string();
            let role_name = record.str_field("role").to_string();
            if role_user == msg.user_id() && role_name == "admin" {
                return err_bad_request("Cannot remove your own admin role");
            }
            role_user
        }
        Err(e) if e.code == ErrorCode::NotFound => {
            return err_not_found("User-role assignment not found");
        }
        Err(e) => {
            return err_internal("Database error", e);
        }
    };

    match db::delete(ctx, USER_ROLES_TABLE, id).await {
        Ok(()) => {
            // P2c: role removal (demotion) is exactly the change this
            // mechanism exists for — bump so a JWT minted with the removed
            // role stops authenticating as that role immediately rather
            // than at its natural expiry.
            if let Err(e) = bump_auth_version(ctx, &role_user).await {
                tracing::error!(
                    user_id = %role_user,
                    error = %e,
                    "role removed but auth_version bump failed"
                );
                return err_internal("Role removed but session invalidation failed", e);
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
        Err(e) if e.code == ErrorCode::NotFound => err_not_found("User-role assignment not found"),
        Err(e) => err_internal("Database error", e),
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
    use wafer_run::{BlockInfo, WaferError};

    use super::*;
    use crate::test_support::{admin_msg, output_is_error, output_json, TestContext};

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
            is_write: bool,
        ) -> Result<(), WaferError> {
            self.inner
                .check_resource_access(resource, resource_type, is_write)
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

    /// Seed a real system role (`is_system: true`) via the shared
    /// `seed_defaults` path and return its row id.
    async fn seed_system_role(ctx: &dyn Context) -> String {
        seed_defaults(ctx).await;
        let records = db::list_all(
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

    #[tokio::test]
    async fn update_role_rejects_mutation_when_guard_read_errors() {
        // Real system role exists in the DB (renaming it would break auth).
        let ctx = TestContext::with_admin().await;
        let role_id = seed_system_role(&ctx).await;
        let failing = FailingGetContext { inner: ctx };

        // Attempt to rename the system role while the protective guard read
        // (db::get) is failing. The mutation must be rejected — not silently
        // let through because the guard couldn't be evaluated.
        let path = format!("/admin/iam/roles/{role_id}");
        let out = handle_update_role(
            &failing,
            &path,
            body_input(serde_json::json!({"name": "renamed-admin"})),
        )
        .await;
        assert!(
            output_is_error(out, "Internal").await,
            "a guard-read infra error must reject the mutation (fail closed)"
        );

        // Verify no rename actually happened — `list` isn't intercepted, so
        // this reads the real row through the same context.
        let records = db::list_all(
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
        let ctx = TestContext::with_admin().await;
        let role_id = seed_system_role(&ctx).await;

        let path = format!("/admin/iam/roles/{role_id}");
        let out = handle_update_role(
            &ctx,
            &path,
            body_input(serde_json::json!({"name": "renamed-admin"})),
        )
        .await;
        assert!(output_is_error(out, "PermissionDenied").await);
    }

    #[tokio::test]
    async fn update_role_missing_row_returns_not_found() {
        let ctx = TestContext::with_admin().await;
        let path = "/admin/iam/roles/does-not-exist".to_string();
        let out = handle_update_role(
            &ctx,
            &path,
            body_input(serde_json::json!({"description": "x"})),
        )
        .await;
        assert!(output_is_error(out, "NotFound").await);
    }

    #[tokio::test]
    async fn update_role_non_system_role_updates_normally() {
        let ctx = TestContext::with_admin().await;
        let data = json_map(serde_json::json!({
            "name": "editor",
            "description": "old",
            "is_system": false,
            "permissions": []
        }));
        let created = db::create(&ctx, ROLES_TABLE, data).await.unwrap();

        let path = format!("/admin/iam/roles/{}", created.id);
        let out = handle_update_role(
            &ctx,
            &path,
            body_input(serde_json::json!({"name": "renamed-editor"})),
        )
        .await;
        let json = output_json(out).await;
        assert_eq!(json["data"]["name"], "renamed-editor");
    }

    /// P2c: assigning a role is a security-relevant grant — it must bump the
    /// target user's auth_version so an already-issued access JWT (minted
    /// with the old, smaller role set) is invalidated instead of keeping its
    /// stale `roles` claim until natural expiry.
    #[tokio::test]
    async fn assign_role_bumps_the_targets_auth_version() {
        use crate::blocks::auth::repo::users;

        let ctx = TestContext::with_auth().await;
        let uid = users::insert(
            &ctx,
            users::NewUser {
                email: "grantee@example.com".into(),
                display_name: "Grantee".into(),
                avatar_url: None,
                role: "user".into(),
            },
        )
        .await
        .unwrap()
        .id;
        assert_eq!(users::auth_version(&ctx, &uid).await.unwrap(), 0);

        let msg = admin_msg("create", "/admin/iam/user-roles");
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

        let ctx = TestContext::with_auth().await;
        let uid = users::insert(
            &ctx,
            users::NewUser {
                email: "demotee@example.com".into(),
                display_name: "Demotee".into(),
                avatar_url: None,
                role: "user".into(),
            },
        )
        .await
        .unwrap()
        .id;

        let msg = admin_msg("create", "/admin/iam/user-roles");
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
        // removal's OWN bump is what this test proves.
        let before_remove = users::auth_version(&ctx, &uid).await.unwrap();

        let remove_path = format!("/admin/iam/user-roles/{role_row_id}");
        let remove_msg = admin_msg("delete", &remove_path);
        let out = handle_remove_role(&ctx, &remove_msg, &remove_path).await;
        assert!(
            !output_is_error(out, "Internal").await,
            "remove must succeed"
        );

        assert_eq!(
            users::auth_version(&ctx, &uid).await.unwrap(),
            before_remove + 1,
            "removing a role must bump the target user's auth_version"
        );
    }
}
