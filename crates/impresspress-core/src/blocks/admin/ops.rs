//! Shared admin-mutation domain layer.
//!
//! Both admin surfaces — the JSON API (`users.rs` / `iam.rs` / `settings.rs`)
//! and the SSR htmx pages (`pages/users.rs` / `pages/variables.rs`) — drive the
//! same handful of privileged operations: disable / enable / delete a user,
//! create / delete a role, create / update a config variable. Historically each
//! surface hand-copied the business rules, and the copies drifted on exactly the
//! things that matter most:
//!
//! * the JSON path issued **zero audit-log rows** for any of these mutations;
//! * the SSR variable create/update path skipped **URL/SSRF validation**;
//! * the SSR variable read path masked secrets on the `sensitive` flag only,
//!   ignoring the SEC-060 `_SECRET` / `_KEY` suffix rule.
//!
//! This module is the single owner of those rules so the surfaces can't diverge
//! again. Every function here performs the guard checks, the validation, the
//! audit-log write, and the database mutation; the callers keep only their
//! response shape (a JSON record vs. an htmx fragment + toast). Guard/validation
//! failures are returned as a ready-to-emit [`OutputStream`] error via the
//! shared `crate::http::err_*` constructors, so both surfaces report failures
//! identically.

use std::collections::HashMap;

use wafer_block::db::{Filter, FilterOp};
use wafer_core::clients::database as db;
use wafer_run::{context::Context, ErrorCode, Message, OutputStream};

use super::{logs::audit_log, settings::VARIABLES_TABLE, ROLES_TABLE, USER_ROLES_TABLE};
/// SSRF URL validator for `InputType::Url` writes. The single implementation
/// lives in [`crate::util::validate_url_value`]; re-exported here so the admin
/// variable create/update paths and the generic settings form
/// (`ui::settings_form::save_settings`) validate through the exact same impl and
/// can't diverge on what a URL value is allowed to be.
pub(super) use crate::util::validate_url_value;
/// `MASKED_VALUE` / `is_sensitive_key`: the single source of truth lives in
/// [`crate::util`] so the generic ConfigVar-driven settings form
/// (`ui::settings_form`) can share it too — masking on a DB `sensitive` flag
/// (or `InputType::Password`) alone leaked a `*_SECRET`/`*_KEY` value
/// whenever a var/row wasn't explicitly marked. Both surfaces (JSON
/// `handle_list*`, the SSR variable tables, and the shared settings form)
/// must agree on this rule; re-exported here so existing `ops::`-qualified
/// call sites in this module tree keep working.
pub(super) use crate::util::{is_sensitive_key, MASKED_VALUE};
use crate::{
    blocks::auth::USERS_TABLE,
    http::{err_bad_request, err_forbidden, err_internal, err_not_found},
    util::RecordExt,
};

/// Bulk-fetch the roles assigned to each of `user_ids` in a single `In`-filter
/// query, bucketed back into a `user_id -> [role]` map.
///
/// Replaces the per-user `list_all(USER_ROLES_TABLE, …)` loop that both the
/// JSON `users::handle_list` / `get_user` paths and the SSR
/// `pages/users.rs::user_row_fragment` re-implemented. The single-row lookup is
/// the `user_ids = [one]` case, so this is the only roles-fetch helper.
///
/// On query failure every requested user maps to an empty role list (the prior
/// per-surface code swallowed the error the same way).
pub(super) async fn fetch_roles(
    ctx: &dyn Context,
    user_ids: &[&str],
) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    if user_ids.is_empty() {
        return out;
    }
    let values: Vec<serde_json::Value> = user_ids
        .iter()
        .map(|id| serde_json::Value::String((*id).to_string()))
        .collect();
    let filters = vec![Filter {
        field: "user_id".to_string(),
        operator: FilterOp::In,
        value: serde_json::Value::Array(values),
    }];
    if let Ok(rows) = db::list_all(ctx, USER_ROLES_TABLE, filters).await {
        for rec in &rows {
            let uid = rec.str_field("user_id").to_string();
            let role = rec.str_field("role").to_string();
            if !uid.is_empty() && !role.is_empty() {
                out.entry(uid).or_default().push(role);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// User mutations
// ---------------------------------------------------------------------------

/// Set the `disabled` flag on a user (true = disable, false = enable), writing
/// an audit-log row. The self-disable guard only applies when disabling.
///
/// Returns the updated [`db::Record`] (password hash stripped) on success.
pub(super) async fn set_user_disabled(
    ctx: &dyn Context,
    msg: &Message,
    user_id: &str,
    disabled: bool,
) -> Result<db::Record, OutputStream> {
    let admin_id = msg.user_id().to_string();
    // Prevent admins from disabling themselves (lockout). Enabling yourself is
    // harmless, so the guard is disable-only.
    if disabled && admin_id == user_id {
        return Err(err_bad_request("Cannot disable your own account"));
    }

    let mut data = HashMap::new();
    data.insert("disabled".to_string(), serde_json::json!(disabled));
    crate::util::stamp_updated(&mut data);

    let record = match db::update(ctx, USERS_TABLE, user_id, data).await {
        Ok(mut record) => {
            record.data.remove("password_hash");
            record
        }
        Err(e) if e.code == ErrorCode::NotFound => return Err(err_not_found("User not found")),
        Err(e) => return Err(err_internal("Database error", e)),
    };

    let action = if disabled {
        "user.disable"
    } else {
        "user.enable"
    };
    audit_log(
        ctx,
        &admin_id,
        action,
        &format!("users/{user_id}"),
        msg.remote_addr(),
    )
    .await;
    Ok(record)
}

/// Soft-delete a user, writing an audit-log row. Rejects self-deletion.
pub(super) async fn delete_user(
    ctx: &dyn Context,
    msg: &Message,
    user_id: &str,
) -> Result<(), OutputStream> {
    let admin_id = msg.user_id().to_string();
    if admin_id == user_id {
        return Err(err_bad_request("Cannot delete your own account"));
    }

    match db::soft_delete(ctx, USERS_TABLE, user_id).await {
        Ok(_) => {}
        Err(e) if e.code == ErrorCode::NotFound => return Err(err_not_found("User not found")),
        Err(e) => return Err(err_internal("Database error", e)),
    }

    audit_log(
        ctx,
        &admin_id,
        "user.delete",
        &format!("users/{user_id}"),
        msg.remote_addr(),
    )
    .await;
    Ok(())
}

/// Apply the whitelisted user-profile fields (`name`, `disabled`,
/// `avatar_url`) from `body`, writing an audit-log row. Enforces the
/// self-disable guard, mirroring [`set_user_disabled`].
///
/// Returns the updated record (password hash stripped).
pub(super) async fn update_user_fields(
    ctx: &dyn Context,
    msg: &Message,
    user_id: &str,
    body: &HashMap<String, serde_json::Value>,
) -> Result<db::Record, OutputStream> {
    let admin_id = msg.user_id().to_string();
    if admin_id == user_id {
        if let Some(disabled) = body.get("disabled") {
            if disabled == &serde_json::Value::Bool(true) || disabled == &serde_json::json!(1) {
                return Err(err_bad_request("Cannot disable your own account"));
            }
        }
    }

    let mut data = HashMap::new();
    for key in &["name", "disabled", "avatar_url"] {
        if let Some(val) = body.get(*key) {
            data.insert(key.to_string(), val.clone());
        }
    }
    crate::util::stamp_updated(&mut data);

    let record = match db::update(ctx, USERS_TABLE, user_id, data).await {
        Ok(mut record) => {
            record.data.remove("password_hash");
            record
        }
        Err(e) if e.code == ErrorCode::NotFound => return Err(err_not_found("User not found")),
        Err(e) => return Err(err_internal("Database error", e)),
    };

    audit_log(
        ctx,
        &admin_id,
        "user.update",
        &format!("users/{user_id}"),
        msg.remote_addr(),
    )
    .await;
    Ok(record)
}

// ---------------------------------------------------------------------------
// Role mutations
// ---------------------------------------------------------------------------

/// Create a role with the given name and optional description, writing an
/// audit-log row. `description` / `permissions` default to empty.
pub(super) async fn create_role(
    ctx: &dyn Context,
    msg: &Message,
    name: &str,
    description: Option<&str>,
    permissions: Option<Vec<String>>,
) -> Result<db::Record, OutputStream> {
    if name.is_empty() {
        return Err(err_bad_request("Role name is required"));
    }
    let admin_id = msg.user_id().to_string();

    let mut data = crate::util::json_map(serde_json::json!({
        "name": name,
        "description": description.unwrap_or_default(),
        "permissions": permissions.unwrap_or_default(),
        "is_system": false,
    }));
    crate::util::stamp_created(&mut data);

    let record = match db::create(ctx, ROLES_TABLE, data).await {
        Ok(record) => record,
        Err(e) => return Err(err_internal("Database error", e)),
    };

    audit_log(
        ctx,
        &admin_id,
        "role.create",
        &format!("roles/{name}"),
        msg.remote_addr(),
    )
    .await;
    Ok(record)
}

/// Delete a role, writing an audit-log row. Rejects deletion of system roles
/// (the `is_system` flag), which would break auth.
pub(super) async fn delete_role(
    ctx: &dyn Context,
    msg: &Message,
    role_id: &str,
) -> Result<(), OutputStream> {
    if role_id.is_empty() {
        return Err(err_bad_request("Missing role ID"));
    }
    let admin_id = msg.user_id().to_string();

    // Protect system roles. A missing row falls through to db::delete, which
    // returns NotFound below.
    if let Ok(role) = db::get(ctx, ROLES_TABLE, role_id).await {
        if role.bool_field("is_system") {
            return Err(err_forbidden("Cannot delete system role"));
        }
    }

    match db::delete(ctx, ROLES_TABLE, role_id).await {
        Ok(()) => {}
        Err(e) if e.code == ErrorCode::NotFound => return Err(err_not_found("Role not found")),
        Err(e) => return Err(err_internal("Database error", e)),
    }

    audit_log(
        ctx,
        &admin_id,
        "role.delete",
        &format!("roles/{role_id}"),
        msg.remote_addr(),
    )
    .await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Variable mutations
// ---------------------------------------------------------------------------

/// Create a config variable, writing an audit-log row. Validates `_URL` keys
/// against [`validate_url_value`] (SSRF). `key` must be non-empty.
pub(super) async fn create_variable(
    ctx: &dyn Context,
    msg: &Message,
    key: &str,
    value: &str,
    name: Option<&str>,
    description: Option<&str>,
    sensitive: bool,
) -> Result<db::Record, OutputStream> {
    if key.is_empty() {
        return Err(err_bad_request("Key is required"));
    }
    let admin_id = msg.user_id().to_string();

    // Validate URL-type keys (SSRF) on both surfaces.
    if key.ends_with("_URL") {
        if let Err(e) = validate_url_value(value) {
            return Err(err_bad_request(&format!("Invalid value for {key}: {e}")));
        }
    }

    let mut data = crate::util::json_map(serde_json::json!({
        "key": key,
        "value": value,
        "name": name.filter(|n| !n.is_empty()).unwrap_or(key),
        "description": description.unwrap_or_default(),
        "sensitive": if sensitive { 1 } else { 0 },
        "updated_by": msg.user_id(),
    }));
    crate::util::stamp_created(&mut data);

    let record = match db::create(ctx, VARIABLES_TABLE, data).await {
        Ok(record) => record,
        Err(e) => return Err(err_internal("Database error", e)),
    };

    audit_log(
        ctx,
        &admin_id,
        "variable.create",
        &format!("variables/{key}"),
        msg.remote_addr(),
    )
    .await;
    Ok(record)
}

/// Fields a variable update may change. `None` leaves the existing column
/// untouched.
#[derive(Default)]
pub(super) struct VariableUpdate<'a> {
    pub value: Option<&'a str>,
    pub description: Option<&'a str>,
}

/// Update a config variable identified by `key` (upsert on the `key` column),
/// writing an audit-log row. Enforces the sensitive-empty guard (a sensitive
/// value can't be cleared — see [`is_sensitive_key`]: the `_SECRET`/`_KEY`
/// suffix rule unioned with the row's stored `sensitive` flag, which covers
/// Password-typed declared vars like `BOOTSTRAP_ADMIN_PASSWORD`) and the
/// `_URL` SSRF validation on both surfaces.
///
/// Returns the upserted record.
pub(super) async fn update_variable(
    ctx: &dyn Context,
    msg: &Message,
    key: &str,
    update: VariableUpdate<'_>,
) -> Result<db::Record, OutputStream> {
    if key.is_empty() {
        return Err(err_bad_request("Missing setting key"));
    }
    let admin_id = msg.user_id().to_string();

    if let Some(value) = update.value {
        // Prevent clearing a sensitive value (would break auth). Sensitivity
        // is the same union the read/masking paths use ([`is_sensitive_key`]):
        // the SEC-060 `_SECRET`/`_KEY` suffix rule OR the row's stored
        // `sensitive` flag — the flag is what marks Password-typed declared
        // vars without the suffix (e.g. `BOOTSTRAP_ADMIN_PASSWORD`, `*_TOKEN`)
        // and ad hoc rows flagged in the UI. The row lookup only happens on
        // the empty-value path; a missing row (upsert-create branch) has no
        // stored secret to wipe, so only the suffix rule applies there.
        if value.is_empty() {
            let stored_flag = match db::get_by_field(
                ctx,
                VARIABLES_TABLE,
                "key",
                serde_json::Value::String(key.to_string()),
            )
            .await
            {
                Ok(record) => record.i64_field("sensitive"),
                Err(e) if e.code == ErrorCode::NotFound => 0,
                Err(e) => return Err(err_internal("Database error", e)),
            };
            if is_sensitive_key(key, stored_flag) {
                return Err(err_bad_request(&format!(
                    "Cannot set {key} to an empty value"
                )));
            }
        }
        // Validate URL-type keys (SSRF) on both surfaces.
        if key.ends_with("_URL") {
            if let Err(e) = validate_url_value(value) {
                return Err(err_bad_request(&format!("Invalid value for {key}: {e}")));
            }
        }
    }

    let mut data = HashMap::new();
    // `key` is `NOT NULL` and supplies the row's identity on the upsert-create
    // branch (PUT to a not-yet-present key). On the update branch it resolves
    // to a no-op (sets `key` to its current value), so it's safe to always
    // include — and required for the create branch to satisfy the constraint.
    data.insert("key".to_string(), serde_json::json!(key));
    if let Some(value) = update.value {
        data.insert("value".to_string(), serde_json::json!(value));
    }
    if let Some(description) = update.description {
        data.insert("description".to_string(), serde_json::json!(description));
    }
    data.insert("updated_by".to_string(), serde_json::json!(msg.user_id()));
    crate::util::stamp_updated(&mut data);

    let record = match db::upsert_by_field(
        ctx,
        VARIABLES_TABLE,
        "key",
        serde_json::Value::String(key.to_string()),
        data,
    )
    .await
    {
        Ok(record) => record,
        Err(e) => return Err(err_internal("Database error", e)),
    };

    audit_log(
        ctx,
        &admin_id,
        "variable.update",
        &format!("variables/{key}"),
        msg.remote_addr(),
    )
    .await;
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    // `is_sensitive_key_honors_flag_and_suffix` lives in `crate::util`'s test
    // module now, alongside the function it tests (moved when
    // `is_sensitive_key`/`MASKED_VALUE` were promoted to the shared
    // single-source location — see the doc comment on the re-export above).

    // --- End-to-end regression tests for the security drifts this module
    // closes. They run the ops functions against the real DatabaseBlock (via
    // TestContext) so they exercise the same statements both surfaces now share.
    use crate::test_support::{admin_msg, TestContext};

    /// Assert an ops call succeeded. `OutputStream` (the `Err` arm) isn't
    /// `Debug`, so `.expect()` can't be used directly.
    #[track_caller]
    fn expect_ok<T>(res: Result<T, OutputStream>) -> T {
        match res {
            Ok(v) => v,
            Err(_) => panic!("expected ops call to succeed, got an error OutputStream"),
        }
    }

    /// Set up a context with the admin schema (variables, roles, audit_logs).
    async fn admin_ctx() -> TestContext {
        let ctx = TestContext::new().await;
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");
        ctx
    }

    /// Count audit-log rows whose `action` matches.
    async fn audit_count(ctx: &dyn Context, action: &str) -> usize {
        let filters = vec![Filter {
            field: "action".to_string(),
            operator: FilterOp::Equal,
            value: serde_json::Value::String(action.to_string()),
        }];
        db::list_all(ctx, super::super::logs::AUDIT_LOGS_TABLE, filters)
            .await
            .map(|r| r.len())
            .unwrap_or(0)
    }

    /// SEC drift: the JSON variable path wrote zero audit rows. Both surfaces
    /// now go through `create_variable` / `update_variable`, which always log.
    #[tokio::test]
    async fn variable_create_and_update_write_audit_rows() {
        let ctx = admin_ctx().await;
        let msg = admin_msg("create", "/admin/settings");

        expect_ok(create_variable(&ctx, &msg, "SITE_NAME", "Acme", None, None, false).await);
        assert_eq!(audit_count(&ctx, "variable.create").await, 1);

        expect_ok(
            update_variable(
                &ctx,
                &msg,
                "SITE_NAME",
                VariableUpdate {
                    value: Some("Acme Two"),
                    description: None,
                },
            )
            .await,
        );
        assert_eq!(audit_count(&ctx, "variable.update").await, 1);
    }

    /// `update_variable` upserts: a PUT to a not-yet-present key must create a
    /// row whose `NOT NULL` `key` column is persisted (the JSON `handle_set`
    /// create-via-PUT contract from main). Regression for the upsert-create
    /// branch omitting `key` and tripping the constraint.
    #[tokio::test]
    async fn update_variable_creates_row_for_new_key() {
        let ctx = admin_ctx().await;
        let msg = admin_msg("update", "/admin/settings");

        // The key does not exist yet → upsert takes the create branch.
        let record = expect_ok(
            update_variable(
                &ctx,
                &msg,
                "NEW_SITE_TAGLINE",
                VariableUpdate {
                    value: Some("Hello"),
                    description: Some("a fresh key"),
                },
            )
            .await,
        );
        assert_eq!(
            record.data.get("key").and_then(|v| v.as_str()),
            Some("NEW_SITE_TAGLINE"),
            "the created row must persist its key column"
        );

        // The row is now findable by `key` (proves the NOT NULL row landed).
        let found = db::get_by_field(
            &ctx,
            VARIABLES_TABLE,
            "key",
            serde_json::Value::String("NEW_SITE_TAGLINE".to_string()),
        )
        .await
        .expect("the upserted variable is findable by key");
        assert_eq!(
            found.data.get("value").and_then(|v| v.as_str()),
            Some("Hello")
        );

        // A second update on the same key takes the update branch (no
        // duplicate row, key unchanged).
        expect_ok(
            update_variable(
                &ctx,
                &msg,
                "NEW_SITE_TAGLINE",
                VariableUpdate {
                    value: Some("Goodbye"),
                    description: None,
                },
            )
            .await,
        );
        let rows = db::list_all(
            &ctx,
            VARIABLES_TABLE,
            vec![Filter {
                field: "key".to_string(),
                operator: FilterOp::Equal,
                value: serde_json::Value::String("NEW_SITE_TAGLINE".to_string()),
            }],
        )
        .await
        .expect("list variables by key");
        assert_eq!(rows.len(), 1, "update must not create a second row");
        assert_eq!(
            rows[0].data.get("value").and_then(|v| v.as_str()),
            Some("Goodbye")
        );
    }

    /// SEC drift: the SSR variable path ran no URL/SSRF validation. Both
    /// surfaces now share the `_URL` check in create/update.
    #[tokio::test]
    async fn variable_url_keys_are_ssrf_validated_on_both_paths() {
        let ctx = admin_ctx().await;
        let msg = admin_msg("create", "/admin/settings");

        // A private-IP URL is rejected on create.
        assert!(create_variable(
            &ctx,
            &msg,
            "WEBHOOK_URL",
            "https://10.0.0.1/x",
            None,
            None,
            false
        )
        .await
        .is_err());
        // ...and on update.
        assert!(update_variable(
            &ctx,
            &msg,
            "WEBHOOK_URL",
            VariableUpdate {
                value: Some("https://192.168.1.1"),
                description: None,
            },
        )
        .await
        .is_err());
        // A public HTTPS URL is accepted.
        assert!(create_variable(
            &ctx,
            &msg,
            "WEBHOOK_URL",
            "https://example.com/hook",
            None,
            None,
            false
        )
        .await
        .is_ok());
    }

    /// A sensitive (`_SECRET` / `_KEY`) value can't be cleared to empty on
    /// either surface (would break auth).
    #[tokio::test]
    async fn sensitive_key_cannot_be_cleared() {
        let ctx = admin_ctx().await;
        let msg = admin_msg("update", "/admin/settings");
        assert!(update_variable(
            &ctx,
            &msg,
            "JWT_SECRET",
            VariableUpdate {
                value: Some(""),
                description: None,
            },
        )
        .await
        .is_err());
    }

    /// The sensitive-empty guard must honor the row's stored `sensitive`
    /// flag, not only the `_SECRET`/`_KEY` suffix — Password-typed declared
    /// vars (e.g. `BOOTSTRAP_ADMIN_PASSWORD`, `*_TOKEN`) are stored with
    /// `sensitive = 1` but carry neither suffix, and clearing one would
    /// break auth just the same.
    #[tokio::test]
    async fn password_typed_var_without_suffix_cannot_be_cleared() {
        let ctx = admin_ctx().await;
        let msg = admin_msg("update", "/admin/settings");

        // Seed the row the way the declared-ConfigVar sync does for
        // `InputType::Password` vars: `sensitive = 1`, suffix-less key.
        expect_ok(
            create_variable(
                &ctx,
                &msg,
                "BOOTSTRAP_ADMIN_PASSWORD",
                "hunter2",
                None,
                None,
                true,
            )
            .await,
        );

        // Clearing it must be rejected...
        assert!(update_variable(
            &ctx,
            &msg,
            "BOOTSTRAP_ADMIN_PASSWORD",
            VariableUpdate {
                value: Some(""),
                description: None,
            },
        )
        .await
        .is_err());

        // ...leaving the stored value untouched.
        let row = db::get_by_field(
            &ctx,
            VARIABLES_TABLE,
            "key",
            serde_json::Value::String("BOOTSTRAP_ADMIN_PASSWORD".to_string()),
        )
        .await
        .expect("seeded row still present");
        assert_eq!(
            row.data.get("value").and_then(|v| v.as_str()),
            Some("hunter2"),
            "rejected clear must not overwrite the stored secret"
        );

        // A var that is neither suffix-sensitive nor flagged can still be
        // cleared (the guard is a union, not a blanket empty-value ban).
        expect_ok(create_variable(&ctx, &msg, "SITE_TAGLINE", "x", None, None, false).await);
        expect_ok(
            update_variable(
                &ctx,
                &msg,
                "SITE_TAGLINE",
                VariableUpdate {
                    value: Some(""),
                    description: None,
                },
            )
            .await,
        );
    }

    /// Self-disable and self-delete guards hold; a successful user mutation
    /// writes an audit row.
    #[tokio::test]
    async fn user_self_mutation_guards_and_audit() {
        let ctx = admin_ctx().await;
        // The user mutations touch the auth `users` table.
        crate::blocks::auth::migrations::apply(&ctx)
            .await
            .expect("apply auth migrations");
        // admin_msg's user is "admin_1".
        let msg = admin_msg("update", "/admin/users/admin_1");

        // Disabling yourself is rejected (and writes no audit row).
        assert!(set_user_disabled(&ctx, &msg, "admin_1", true)
            .await
            .is_err());
        // Deleting yourself is rejected.
        assert!(delete_user(&ctx, &msg, "admin_1").await.is_err());
        assert_eq!(audit_count(&ctx, "user.disable").await, 0);

        // Seed another user, then disable them — succeeds and logs.
        let mut data = HashMap::new();
        data.insert("id".to_string(), serde_json::json!("u2"));
        data.insert("email".to_string(), serde_json::json!("u2@example.com"));
        data.insert("display_name".to_string(), serde_json::json!("User Two"));
        crate::util::stamp_created(&mut data);
        db::create(&ctx, USERS_TABLE, data)
            .await
            .expect("seed user");

        expect_ok(set_user_disabled(&ctx, &msg, "u2", true).await);
        assert_eq!(audit_count(&ctx, "user.disable").await, 1);
    }
}
