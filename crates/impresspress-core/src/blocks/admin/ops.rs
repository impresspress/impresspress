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

use wafer_core::clients::database as db;
use wafer_run::{context::Context, ErrorCode, Message, OutputStream};

use super::{logs::audit_log, ROLES_TABLE};
/// The config-value write rule. The single implementation lives in
/// [`crate::util::validate_config_value`]; re-exported here so the admin
/// variable create/update paths, the `config.set` writer and the generic
/// settings form (`ui::settings_form::save_settings`) validate through the
/// exact same impl and can't diverge on what a value is allowed to be.
pub(super) use crate::util::validate_config_value;
/// `MASKED_VALUE` / `is_sensitive_key`: the single source of truth lives in
/// [`crate::util`] so the generic ConfigVar-driven settings form
/// (`ui::settings_form`) can share it too — masking on a DB `sensitive` flag
/// (or `InputType::Password`) alone leaked a `*_SECRET`/`*_KEY` value
/// whenever a var/row wasn't explicitly marked, and masking on the flag plus
/// the suffix alone leaked a declared password var that spells neither.
/// Every surface (JSON `handle_list*`, the SSR variable tables and the edit
/// modal, the shared settings form, the edge-cache exclusion and the export
/// filter) must agree on this rule; re-exported here so existing
/// `ops::`-qualified call sites in this module tree keep working.
pub(super) use crate::util::{is_masked_submission, is_sensitive_key, MASKED_VALUE};
use crate::{
    blocks::{
        auth::{
            bump_auth_version,
            repo::users::{self, AdminUserPatch, UserRow},
        },
        crud::{db_error, db_error_internal, taken_key_or_db_error, TakenKey},
    },
    http::{err_bad_request, err_forbidden},
    platform_state::{
        user_roles,
        variables::{self, NewVariable, VariablePatch, VariableRow},
    },
    util::RecordExt,
};

/// Bulk-fetch the roles assigned to each of `user_ids` in a single `In`-filter
/// query, bucketed back into a `user_id -> [role]` map.
///
/// Replaces the per-user roles-table loop that both the JSON
/// `users::handle_list` / `get_user` paths and the SSR
/// `pages/users.rs::user_row_fragment` re-implemented. The single-row lookup is
/// the `user_ids = [one]` case, so this is the only roles-fetch helper.
///
/// A failed query is the caller's error to report. A user with no roles is
/// absent from the map, so an empty list must never stand in for "could not
/// read": the Users tab would show "no roles" for an admin, and the JSON API
/// would answer `roles: []`.
pub(super) async fn fetch_roles(
    ctx: &dyn Context,
    user_ids: &[&str],
) -> Result<HashMap<String, Vec<String>>, wafer_run::WaferError> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    if user_ids.is_empty() {
        return Ok(out);
    }
    for row in user_roles::list_for_users(ctx, user_ids).await? {
        out.entry(row.user_id).or_default().push(row.role);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// User mutations
// ---------------------------------------------------------------------------

/// Set the `disabled` flag on a user (true = disable, false = enable), writing
/// an audit-log row. The self-disable guard only applies when disabling.
///
/// Returns the updated [`UserRow`] on success.
pub(super) async fn set_user_disabled(
    ctx: &dyn Context,
    msg: &Message,
    user_id: &str,
    disabled: bool,
) -> Result<UserRow, OutputStream> {
    let admin_id = msg.user_id().to_string();
    // Prevent admins from disabling themselves (lockout). Enabling yourself is
    // harmless, so the guard is disable-only.
    if disabled && admin_id == user_id {
        return Err(err_bad_request("Cannot disable your own account"));
    }

    let row = match users::set_disabled(ctx, user_id, disabled).await {
        Ok(row) => row,
        Err(e) => return Err(db_error(e, "User not found", "Database error")),
    };

    // P2c: a disable/enable is a lifecycle change existing access JWTs must
    // not survive — bump auth_version so they stop authenticating on the
    // next request (see `crate::crypto::extract_auth_meta`). The mutation
    // has already landed, so a failed bump must not be reported as success.
    if let Err(e) = bump_auth_version(ctx, user_id).await {
        tracing::error!(
            user_id = %user_id,
            error = %e,
            "user disabled/enabled but auth_version bump failed"
        );
        return Err(db_error_internal(
            e,
            "User updated but session invalidation failed",
        ));
    }

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
    Ok(row)
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

    match users::soft_delete(ctx, user_id).await {
        Ok(()) => {}
        Err(e) => return Err(db_error(e, "User not found", "Database error")),
    }

    // P2c: same reasoning as `set_user_disabled` — a soft-delete must
    // invalidate already-issued access JWTs, not just refresh tokens.
    if let Err(e) = bump_auth_version(ctx, user_id).await {
        tracing::error!(
            user_id = %user_id,
            error = %e,
            "user deleted but auth_version bump failed"
        );
        return Err(db_error_internal(
            e,
            "User deleted but session invalidation failed",
        ));
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

/// Apply the fields an admin may change on an account — the whitelist is
/// [`AdminUserPatch`] itself — writing an audit-log row. Enforces the
/// self-disable guard, mirroring [`set_user_disabled`].
///
/// Returns the updated row.
pub(super) async fn update_user_fields(
    ctx: &dyn Context,
    msg: &Message,
    user_id: &str,
    body: &HashMap<String, serde_json::Value>,
) -> Result<UserRow, OutputStream> {
    let admin_id = msg.user_id().to_string();
    let patch = AdminUserPatch::from_body(body);
    if admin_id == user_id && patch.disabled == Some(true) {
        return Err(err_bad_request("Cannot disable your own account"));
    }

    let row = match users::patch_admin_fields(ctx, user_id, &patch).await {
        Ok(row) => row,
        Err(e) => return Err(db_error(e, "User not found", "Database error")),
    };

    // P2c: this whitelist can also flip `disabled` (the SSR path uses
    // `set_user_disabled` directly, but this JSON path is reachable too) —
    // bump auth_version the same way whenever it does, so a PATCH that
    // disables a user can't bypass JWT invalidation just because it went
    // through the generic field-update endpoint instead.
    if patch.touches_disabled() {
        if let Err(e) = bump_auth_version(ctx, user_id).await {
            tracing::error!(
                user_id = %user_id,
                error = %e,
                "user updated but auth_version bump failed"
            );
            return Err(db_error_internal(
                e,
                "User updated but session invalidation failed",
            ));
        }
    }

    audit_log(
        ctx,
        &admin_id,
        "user.update",
        &format!("users/{user_id}"),
        msg.remote_addr(),
    )
    .await;
    Ok(row)
}

// ---------------------------------------------------------------------------
// Role mutations
// ---------------------------------------------------------------------------

/// Whether a role called `name` exists — the check `iam::handle_assign_role`
/// makes before granting one. `roles.name` is UNIQUE, so one row is all there
/// can be and a `NotFound` from the lookup is the "no such role" answer rather
/// than a failure.
pub(super) async fn role_name_taken(
    ctx: &dyn Context,
    name: &str,
) -> Result<bool, wafer_run::WaferError> {
    match db::get_by_field(
        ctx,
        ROLES_TABLE,
        "name",
        serde_json::Value::String(name.to_string()),
    )
    .await
    {
        Ok(_) => Ok(true),
        Err(e) if e.code == ErrorCode::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

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
        // `roles.name` is UNIQUE: a second role of the same name is a 409, not
        // a 500. See [`taken_key_or_db_error`].
        Err(e) => {
            return Err(taken_key_or_db_error(
                e,
                TakenKey::new("role", "name", name),
                "Database error",
            ))
        }
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

/// What [`delete_role`] did. Either way the role row is gone — a delete that
/// did not happen is an `Err`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RoleDeleted {
    /// The role and every grant of it are gone.
    Clean,
    /// The role is gone, but the revocation pass that runs after its row was
    /// deleted failed before it removed the grants, so a grant assigned while
    /// the delete was in flight may have outlived it.
    ///
    /// Such a grant is not inert. It keeps putting the role's name into every
    /// token its holder is minted, and creating a role of the same name again
    /// revives it: the new role is held by that user without anyone having
    /// assigned it. Nothing can revoke it through the role any more, since the
    /// role it names no longer exists to be deleted again — it has to be
    /// removed from its holder individually.
    ///
    /// `holders` is who the pass found holding a grant of it when it read
    /// them, or `None` when that read is what failed. The failure, and the
    /// holders when known, are logged and recorded on the audit row.
    LateGrantsNotRevoked { holders: Option<Vec<String>> },
    /// The role is gone and the pass after the delete removed every grant it
    /// found, but then stopped invalidating sessions part-way: `holders` is
    /// the holder whose invalidation failed and every one after it, whose
    /// sessions were not invalidated a second time (those before it were). A
    /// token one of them was minted after their first invalidation and before
    /// the revoke still carries the role until it expires. Logged and audited.
    LateSessionsNotInvalidated { holders: Vec<String> },
}

/// Delete a role, revoking every grant of it, writing an audit-log row.
/// Rejects deletion of system roles (the `is_system` flag), which would break
/// auth.
///
/// The grants have to go, and before the role does. `user_roles.role` stores
/// the role NAME, and the auth block builds a token's `roles` claim from those
/// rows without consulting the role definitions — so a grant left behind
/// keeps putting the deleted role's name into every token its holder is
/// minted, and a role later created under the same name silently re-attaches
/// to it. [`revoke_every_grant_of`] says in what order, and what a failure at
/// each step leaves behind.
///
/// A second revocation pass runs after the role row is gone. An assign that
/// passed `iam::handle_assign_role`'s "is this a defined role" check just
/// before the role was deleted can land its grant after the first pass; once
/// the role row is gone no assign can pass that check, so the second pass
/// takes whatever slipped in. (An assign whose check ran before the role was
/// deleted and whose insert lands after this second pass is still possible
/// in principle — the check and the insert are two statements.)
///
/// A failure of that second pass is not reported as a failed delete: the
/// role IS deleted, and answering with an error would tell the caller it is
/// still there. It is [`RoleDeleted::LateGrantsNotRevoked`] or
/// [`RoleDeleted::LateSessionsNotInvalidated`] instead, naming the holders
/// the pass found, and the audit row for the deletion is written either way.
pub(super) async fn delete_role(
    ctx: &dyn Context,
    msg: &Message,
    role_id: &str,
) -> Result<RoleDeleted, OutputStream> {
    if role_id.is_empty() {
        return Err(err_bad_request("Missing role ID"));
    }
    let admin_id = msg.user_id().to_string();

    // The guard read must fail closed, for the same reason
    // `handle_update_role`'s does: an infra error treated as "not a system
    // role" would fall through to the delete and drop the `admin` role. It is
    // also where the role's name comes from, which the revocation needs, so a
    // missing role is answered here as the 404 it is.
    let role = match db::get(ctx, ROLES_TABLE, role_id).await {
        Ok(role) => role,
        Err(e) => return Err(db_error(e, "Role not found", "Database error")),
    };
    if role.bool_field("is_system") {
        return Err(err_forbidden("Cannot delete system role"));
    }
    let name = role.str_field("name").to_string();

    let revoked = match revoke_every_grant_of(ctx, &name).await {
        Ok(n) => n,
        Err(failure) => return Err(failure.before_the_role_is_deleted()),
    };

    match db::delete(ctx, ROLES_TABLE, role_id).await {
        Ok(()) => {}
        Err(e) => return Err(db_error(e, "Role not found", "Database error")),
    }

    let (outcome, detail) = match revoke_every_grant_of(ctx, &name).await {
        Ok(late) => (
            RoleDeleted::Clean,
            format!("grants revoked: {}", revoked + late),
        ),
        Err(failure) => {
            let holders = failure
                .holders
                .as_deref()
                .map_or_else(|| "could not be listed".to_string(), |h| h.join(", "));
            tracing::error!(
                role = %name,
                step = failure.step.describe(),
                holders = %holders,
                error = %failure.error,
                "role deleted, but the revocation pass after the delete failed; a grant \
                 assigned while the delete was in flight may outlive the role"
            );
            let mut detail = format!(
                "grants revoked: {revoked}; the revocation pass after the delete failed while \
                 it {}",
                failure.step.describe()
            );
            if let Some(found) = &failure.holders {
                detail.push_str(&format!("; holders it found: {}", found.join(", ")));
            }
            let outcome = match (failure.step, failure.holders) {
                (RevokeStep::BumpAfter, holders) => RoleDeleted::LateSessionsNotInvalidated {
                    holders: holders.unwrap_or_default(),
                },
                (_, holders) => RoleDeleted::LateGrantsNotRevoked { holders },
            };
            (outcome, detail)
        }
    };

    audit_log(
        ctx,
        &admin_id,
        "role.delete",
        &format!("roles/{role_id} (name: {name}; {detail})"),
        msg.remote_addr(),
    )
    .await;
    Ok(outcome)
}

/// The step of [`revoke_every_grant_of`] that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RevokeStep {
    /// Reading the grants. Nothing was written.
    Read,
    /// Invalidating a holder's sessions before the revoke. No grant was
    /// removed.
    BumpBefore,
    /// Removing the grants.
    Revoke,
    /// Invalidating a holder's sessions after the revoke. The grants are gone.
    BumpAfter,
}

impl RevokeStep {
    /// What the pass was doing, as a phrase for a log line or audit row.
    fn describe(self) -> &'static str {
        match self {
            Self::Read => "was reading the grants",
            Self::BumpBefore => "was invalidating sessions before revoking the grants",
            Self::Revoke => "was revoking the grants",
            Self::BumpAfter => "was invalidating sessions after revoking the grants",
        }
    }
}

/// A failed [`revoke_every_grant_of`]: which step, the holders it concerns,
/// and the error it met. `holders` is every holder the pass read — for
/// [`RevokeStep::BumpAfter`], only the one whose invalidation failed and those
/// after it, since the ones before it were invalidated — and `None` when
/// reading them is what failed.
struct RevokeFailure {
    step: RevokeStep,
    holders: Option<Vec<String>>,
    error: wafer_run::WaferError,
}

impl RevokeFailure {
    /// The response for a failure of the pass that runs while the role row
    /// still exists — so the delete did not happen, and deleting again is
    /// the retry.
    fn before_the_role_is_deleted(self) -> OutputStream {
        match self.step {
            RevokeStep::Read => db_error_internal(self.error, "Database error"),
            RevokeStep::BumpBefore => {
                db_error_internal(self.error, "Role not deleted: session invalidation failed")
            }
            RevokeStep::Revoke => db_error_internal(
                self.error,
                "Role not deleted: its grants could not be revoked",
            ),
            RevokeStep::BumpAfter => db_error_internal(
                self.error,
                "Role not deleted: its grants were revoked but session invalidation failed",
            ),
        }
    }
}

/// Remove every `user_roles` row naming `role` and invalidate each holder's
/// access tokens, returning how many rows went.
///
/// Bump, delete, bump — so a failure at each step leaves something a retry
/// can finish:
///
/// 1. Every holder's auth version is bumped while their grant still exists.
///    If a bump fails here nothing has been removed, the role is still
///    there, and deleting it again finds every holder again.
/// 2. Every grant goes in one statement ([`user_roles::revoke_role`]), which
///    also takes a grant written after the read in step 1.
/// 3. Every holder is bumped again. A refresh that landed between their
///    first bump and the delete minted a token that still names the role;
///    this is what invalidates it. A failure here is reported, but the
///    grants are gone, so a retry cannot find these holders — what remains
///    is only a token minted inside that window, until it expires.
///
/// A holder whose grant was written after step 1's read is not bumped. Their
/// grant is still removed, and the assign that wrote it bumped them, so the
/// only token that can still carry the role is one minted between that
/// assign and step 2.
async fn revoke_every_grant_of(ctx: &dyn Context, role: &str) -> Result<i64, RevokeFailure> {
    let grants = user_roles::list_by_role(ctx, role)
        .await
        .map_err(|error| RevokeFailure {
            step: RevokeStep::Read,
            holders: None,
            error,
        })?;
    let mut holders: Vec<&str> = grants.iter().map(|g| g.user_id.as_str()).collect();
    holders.sort_unstable();
    holders.dedup();
    let failed = |step| {
        let holders = holders.iter().map(|h| h.to_string()).collect();
        move |error| RevokeFailure {
            step,
            holders: Some(holders),
            error,
        }
    };

    bump_each(ctx, &holders)
        .await
        .map_err(|(_, error)| failed(RevokeStep::BumpBefore)(error))?;
    let revoked = user_roles::revoke_role(ctx, role)
        .await
        .map_err(failed(RevokeStep::Revoke))?;
    // The holders before the failing one WERE invalidated; only the failing
    // one and those after it were not, so they are the ones reported.
    bump_each(ctx, &holders)
        .await
        .map_err(|(at, error)| RevokeFailure {
            step: RevokeStep::BumpAfter,
            holders: Some(holders[at..].iter().map(|h| h.to_string()).collect()),
            error,
        })?;
    Ok(revoked)
}

/// Bump each of `user_ids`' auth version, stopping at the first failure and
/// returning its index: every holder before it was bumped, it and every one
/// after it were not.
async fn bump_each(
    ctx: &dyn Context,
    user_ids: &[&str],
) -> Result<(), (usize, wafer_run::WaferError)> {
    for (at, &user_id) in user_ids.iter().enumerate() {
        if let Err(e) = bump_auth_version(ctx, user_id).await {
            tracing::error!(
                user_id = %user_id,
                error = %e,
                "role delete: auth_version bump failed"
            );
            return Err((at, e));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Variable mutations
// ---------------------------------------------------------------------------

/// Refuse a key the runtime owns rather than storing an inert row.
///
/// `blocks::config` never serves infrastructure keys (`IMPRESSPRESS_*` without
/// `__`) or internal adapter keys (`__…__`) from the variables table — the
/// browser's `__IMPRESSPRESS_RUNTIME_KIND__ = "browser"` marker is what keeps
/// Stripe secret-key operations off in a visitor's browser. A row under one of
/// those keys is therefore a setting the admin can see and edit and that no
/// reader will ever honour, which is the silent no-op this program keeps
/// removing. Both write surfaces funnel through here.
///
/// The JWT secret is deliberately NOT refused, and the asymmetry is worth
/// stating because it is target-dependent:
///
/// - on native, `seed_and_load` runs `seed_jwt_secret` and `impresspress_server::build_native_runtime`
///   takes the boot map's secret from the loaded vars, so this row IS the next
///   boot's signing key. Editing it is a real operator action — the row even
///   carries "Rotating this secret invalidates every issued session" — and
///   refusing it here would delete a working capability;
/// - on Cloudflare the secret is a worker binding (`PROTECTED_ENV_KEYS`,
///   consumed by `runtime_build`), and `CfDeployBootHooks::seed_and_load` runs
///   only `seed_auto_generated`, which never covers this key. The row is
///   therefore inert there: an edit is accepted and never takes effect.
///
/// That Cloudflare inertness predates this guard and is not something a
/// target-agnostic write surface can decide. Refusing the key everywhere would
/// trade a real native capability for it. Surfacing it to the operator — a
/// warning on the Variables page for a deployment whose secret comes from the
/// environment — is the fix, and is not attempted here.
///
/// `ui::settings_form`'s `CONFIG_SET` path does refuse this key, because no
/// caller there legitimately writes it: it saves declared block and shared
/// vars only. See the note in `blocks::config::write`.
fn reject_runtime_owned_key(key: &str) -> Result<(), OutputStream> {
    if crate::config_vars::is_runtime_owned_key(key) {
        return Err(err_bad_request(&format!(
            "{key} is set by the runtime, not stored configuration; it cannot be created or edited here"
        )));
    }
    Ok(())
}

/// Whether `key` is a provisioning credential that has already been SPENT, and
/// so may be cleared through the admin surface.
///
/// The bootstrap password is spent the moment provisioning succeeds:
/// `auth::bootstrap::run` early-returns once `wafer_run__auth__users` is
/// non-empty, and the password survives as an argon2 hash in
/// `local_credentials`, so the stored plaintext is a dead copy.
///
/// The bootstrap TOKEN is the case the static
/// [`crate::config_vars::is_provisioning_only_key`] cannot answer, and the same
/// trap the password exemption exists to avoid was still set for it. Its
/// branch of `bootstrap::run` creates NO user, so while `users` is empty every
/// boot re-reads the plaintext and mints a fresh 24h `bootstrap_tokens` row —
/// clearing it then strands the deployment with no admin path, and Cloudflare
/// has no process environment to re-seed from. Once an admin user exists the
/// token is redeemed and inert, and `into_row` flags it while
/// `delete_variable` and `key_is_deletable` both refuse a declared
/// `WAFER_RUN_SHARED__*` row — so without this it could never be cleared or
/// deleted by any route. "Has the token been redeemed" is a checkable
/// condition, not an unknowable one, so it is checked.
///
/// A read failure answers `false`: not knowing whether the token is still live
/// keeps the guard on, which costs an operator one refused clear rather than a
/// lockout.
///
/// The token is handled HERE rather than widened into
/// `is_provisioning_only_key` because that predicate is also consulted by
/// `blocks::config`'s `CONFIG_SET`, which runs over the raw `DatabaseService`
/// with no `Context` to count users through. Widening it there would have
/// exempted an UNREDEEMED token on a surface that cannot check — the narrower
/// static rule keeps that path refusing, which is the safe answer.
async fn is_clearable_provisioning_credential(ctx: &dyn Context, key: &str) -> bool {
    if crate::config_vars::is_provisioning_only_key(key) {
        return true;
    }
    if key != crate::blocks::auth::config::BOOTSTRAP_ADMIN_TOKEN_KEY {
        return false;
    }
    // Redeemed exactly when an admin user exists.
    matches!(crate::blocks::auth::repo::users::count(ctx).await, Ok(n) if n > 0)
}

/// Hand a config key back to the process environment: release its pin so the
/// next boot seeds it from the environment again, and write an audit-log row.
///
/// The supported exit from a pinned key, and the reason admin-wins is not a
/// one-way door. The two routes that look like they should work do not:
/// [`delete_variable`] refuses every declared `WAFER_RUN_SHARED__*` row (PR
/// #71, and left alone — see below), and [`update_variable`] stamps
/// `updated_by` on every write, so clearing the value re-pins the row it was
/// meant to release.
///
/// Chosen over relaxing `delete_variable`'s refusal because the two do
/// different things and only this one says what the operator means. Deleting a
/// declared shared var throws the metadata away and leaves the next boot to
/// re-create the row from the declared default — a bigger, lossier action that
/// also reverts the value even when there is no export to take over. This
/// clears exactly the marker, leaves the value in place until a boot actually
/// re-seeds it, and so is reversible right up to the restart.
///
/// "Release" rather than "clear": `variables::reset_to_environment` records
/// `RELEASED_TO_ENV_SENTINEL`, because an emptied marker is indistinguishable
/// from a row nothing has ever spoken for — which is exactly what the one-time
/// upgrade transition acts on, and would undo this.
pub(super) async fn reset_variable_to_environment(
    ctx: &dyn Context,
    msg: &Message,
    key: &str,
) -> Result<(), OutputStream> {
    release_to_environment(ctx, msg, key, ReleaseGuard::AnyPin)
        .await
        .map(|_| ())
}

/// What the row has to look like AT THE MOMENT OF THE WRITE for a release to
/// proceed — not at the moment the caller chose the key.
///
/// The two surfaces want different answers, so the precondition is a parameter
/// rather than a rule baked into the one function they share.
enum ReleaseGuard {
    /// The per-key control: release the row whatever claims it.
    ///
    /// Deliberately unconditional. An admin handing back their OWN edit is what
    /// that control is for — it is the documented exit from a pin, the one the
    /// boot WARN and `RELEASE.md` both name — so a pin check here would delete
    /// a working capability. An absent row is the caller naming a key that is
    /// not there, and answers 404.
    AnyPin,
    /// The bulk release: proceed only while the row is still
    /// [`variables::Pin::PreUpgrade`].
    ///
    /// [`variables::PinnedAtUpgrade`] proves the row was an upgrade pin when
    /// the SET WAS READ. The writes then happen one at a time, and an admin
    /// edit can land in between — after which releasing that row would clear a
    /// decision a person had just made, the one thing the bulk action must
    /// never do. Re-reading is free here because the existence check already
    /// fetches the row.
    ///
    /// A row that no longer qualifies is SKIPPED rather than reported: one
    /// concurrent edit must not fail the release of the other nine keys, and
    /// the skipped key is still pinned afterwards, so the next press collects
    /// nothing for it and the operator sees the count they actually got.
    ///
    /// It NARROWS the window; it does not close it. This is a re-read followed
    /// by a write, not a conditional write, so an edit landing between
    /// [`release_to_environment`]'s `get_by_key` and its
    /// `variables::reset_to_environment` still loses its pin — the value
    /// survives, but `updated_by` becomes `RELEASED_TO_ENV_SENTINEL` and the
    /// next boot seeds over it. A conditional write is not available:
    /// `db::update` addresses a row by id with no predicate, and
    /// `variables::upsert_by_key` is itself read-then-update-by-id, as is
    /// [`update_variable`]. The gain is real and is the whole point — the window
    /// shrinks from the entire loop (every key's read, every earlier key's
    /// write) to one round trip on the key being written.
    StillPinnedAtUpgrade,
}

/// The shared body of both release surfaces. `Ok(true)` when the row was
/// released, `Ok(false)` when `guard` declined it.
async fn release_to_environment(
    ctx: &dyn Context,
    msg: &Message,
    key: &str,
    guard: ReleaseGuard,
) -> Result<bool, OutputStream> {
    if key.is_empty() {
        return Err(err_bad_request("Missing setting key"));
    }
    // Bound rather than discarded: under `StillPinnedAtUpgrade` the row's pin
    // as it stands NOW is the precondition, and this read is the last look at
    // it before the write below. The gap between the two is the residual window
    // that guard's doc describes.
    let row = match variables::get_by_key(ctx, key).await {
        Ok(Some(row)) => row,
        Ok(None) => {
            return match guard {
                ReleaseGuard::AnyPin => Err(crate::http::err_not_found("Setting not found")),
                // Deleted inside the window. Nothing to release and nothing
                // wrong, for the same reason a re-pinned row is skipped.
                ReleaseGuard::StillPinnedAtUpgrade => Ok(false),
            };
        }
        Err(e) => return Err(db_error_internal(e, "Database error")),
    };
    if matches!(guard, ReleaseGuard::StillPinnedAtUpgrade)
        && variables::pin_of(&row) != Some(variables::Pin::PreUpgrade)
    {
        return Ok(false);
    }
    if let Err(e) = variables::reset_to_environment(ctx, key).await {
        return Err(db_error_internal(e, "Database error"));
    }
    audit_log(
        ctx,
        msg.user_id(),
        "variable.reset_to_environment",
        &format!("variables/{key}"),
        msg.remote_addr(),
    )
    .await;
    Ok(true)
}

/// Hand back every key the one-time upgrade transition pinned, and nothing
/// else. Returns the keys actually released, in the order they were — which is
/// the selection minus anything [`ReleaseGuard::StillPinnedAtUpgrade`] declined
/// on the way through.
///
/// The upgrade boot pins precisely the keys whose stored value disagreed with
/// an export — the keys an operator had configured — so on a deployment that
/// has been administered at all this is several keys, not one, and the per-key
/// control is one confirm dialog each before a single restart.
///
/// **It does not release a [`variables::Pin::AdminEdit`] row**, and that rests
/// on two mechanisms answering two different questions, because the selection
/// and the writes happen at different times:
///
/// * WHICH KEYS — the SELECTION, and a property of the types rather than of
///   this loop. The only thing it can hand to [`release_to_environment`] is a
///   [`variables::PinnedAtUpgrade`], which cannot be built from a key or from a
///   caller's rows: see that type's doc for the three separate things that make
///   it unforgeable and for why
///   [`variables::count_pinned_at_upgrade`] returns a number rather than values.
/// * WHICH ROW — the WRITE. The type above is evidence about a read that has
///   already happened, so it says nothing about the row a given write is about
///   to change; an admin edit can land in between.
///   [`ReleaseGuard::StillPinnedAtUpgrade`] re-reads the row and skips it unless
///   it is still an upgrade pin. That is what actually speaks for the row, and
///   it is a re-read rather than a conditional write — see the guard's own doc
///   for the residual window it narrows but cannot close.
///
/// An admin edit winning permanently is rule 2 of the precedence contract, and
/// a bulk control that quietly cleared one would be a worse defect than the
/// clicking it saves.
///
/// One audit row PER KEY, written by the per-key path itself, carrying the same
/// `variable.reset_to_environment` action and `variables/{key}` resource a
/// single release writes. Both halves are deliberate: the outcome is identical
/// per key, so an operator asking the audit log who released a given key has to
/// find it whichever control was used — `logs::handle_list` filters `resource`
/// with `LIKE` and `action` with equality, and an aggregate row would answer
/// neither query with the key it hid inside a list.
///
/// A failure part way through leaves the keys already released released, and
/// reports the error. That is safe because the action is idempotent: a released
/// row is no longer [`variables::Pin::PreUpgrade`], so it is not in the set the
/// next press collects, and pressing again retries exactly the remainder.
pub(super) async fn release_keys_pinned_at_upgrade(
    ctx: &dyn Context,
    msg: &Message,
) -> Result<Vec<String>, OutputStream> {
    let pinned = match variables::keys_pinned_at_upgrade(ctx).await {
        Ok(keys) => keys,
        Err(e) => return Err(db_error_internal(e, "Database error")),
    };
    release_each(ctx, msg, &pinned).await
}

/// [`release_keys_pinned_at_upgrade`] over a set the caller has ALREADY
/// selected.
///
/// Split out so a test can put something between the selection and the writes —
/// the window [`ReleaseGuard::StillPinnedAtUpgrade`] exists to narrow, and
/// otherwise unreachable, the loop being sequential with no injection point.
///
/// Taking a `&[variables::PinnedAtUpgrade]` rather than keys widens nothing:
/// `platform_state::variables` exposes no way to build one from a key or from
/// caller-supplied rows, so a caller here cannot name an arbitrary row any more
/// than the handler can. That is a property of THAT module, not of this
/// signature — it was briefly lost when a row-taking selector was made public
/// there — so the reasoning lives on `variables::PinnedAtUpgrade` and this is
/// only pointing at it.
pub(super) async fn release_each(
    ctx: &dyn Context,
    msg: &Message,
    pinned: &[variables::PinnedAtUpgrade],
) -> Result<Vec<String>, OutputStream> {
    let mut released = Vec::with_capacity(pinned.len());
    for key in pinned {
        if release_to_environment(ctx, msg, key.key(), ReleaseGuard::StillPinnedAtUpgrade).await? {
            released.push(key.key().to_string());
        }
    }
    Ok(released)
}

/// Delete a config variable, writing an audit-log row.
///
/// Shared by the JSON surface (`settings::handle_delete`) and the Variables
/// page's row control, so the two refuse the same keys and leave the same
/// trail. The JSON path wrote no audit row at all before this: variable
/// creation and update were audited, deletion was not.
///
/// `WAFER_RUN_SHARED__*` is refused, as it always was on the JSON path: those
/// are declared centrally in `config_vars::shared_config_vars()` and re-seeded
/// on the next boot, so deleting one is a no-op that looks like a change.
///
/// Deliberately NOT gated on [`reject_runtime_owned_key`]. Creating or editing
/// a runtime-owned row is refused because the runtime owns that value — but a
/// row that already exists (a legacy write, or a seed bundle from before
/// `dev::data_snapshot::import` learned to refuse them) is exactly what an
/// operator needs to be able to remove. Refusing it here would leave the row
/// permanent, which is the gap this function closes.
pub(super) async fn delete_variable(
    ctx: &dyn Context,
    msg: &Message,
    key: &str,
) -> Result<(), OutputStream> {
    if key.is_empty() {
        return Err(err_bad_request("Missing setting key"));
    }
    // The JWT signing secret. Refused because deleting it is not reversible in
    // the way the confirm dialog implies: nothing breaks until the next boot,
    // when `seed_jwt_secret`'s `insert_if_absent` mints a DIFFERENT secret and
    // every issued session JWT and CSRF token stops verifying. The row also
    // reaches the Variables page's "unowned" table — the auth block never
    // declares it as a `ConfigVar` (see `variables::seed_jwt_secret`) — so
    // without this it sits under a trash icon beside the words "legacy or
    // manually created".
    //
    // Note this is NARROWER than `config_vars::is_instance_owned_key`, which
    // also covers runtime-owned keys. Those stay deletable on purpose: a
    // stored `__…__` or `IMPRESSPRESS_*` row is inert (the boot map answers
    // those keys whatever the table holds) and removing one is exactly the
    // cleanup path this function exists to provide.
    if key == crate::blocks::auth::JWT_SECRET_KEY {
        return Err(err_bad_request(&format!(
            "Cannot delete {key}: it is this instance's JWT signing secret, and the next boot \
             would generate a different one, invalidating every session"
        )));
    }
    // Shared vars are refused only while they are still DECLARED. The reason
    // is that they are re-seeded from `shared_config_vars()` on the next boot,
    // so deleting one is a no-op dressed as a change — but that argument stops
    // holding the moment a key is renamed or dropped from that list. Nothing
    // re-seeds a stale row, and it would otherwise be dead data no surface
    // could remove, which is the gap this function closes.
    if key.starts_with("WAFER_RUN_SHARED__")
        && crate::config_vars::shared_config_vars()
            .iter()
            .any(|declared| declared.key == key)
    {
        return Err(err_bad_request(&format!(
            "Cannot delete shared system variable: {key}"
        )));
    }

    // Through `crud::db_error`, not `err_internal`: it is the one classifier
    // allowed to map a database error by hand, and it preserves the
    // distinctions a caller can act on — a WRAP denial stays a sanitized 403
    // rather than flattening to "Internal server error". The JSON surface
    // mapped errors this way before the guard moved here, and
    // `a_denied_settings_delete_is_403` pins it.
    let row = match variables::get_by_key(ctx, key).await {
        Ok(Some(row)) => row,
        Ok(None) => return Err(crate::http::err_not_found("Setting not found")),
        Err(e) => {
            return Err(crate::blocks::crud::db_error(
                e,
                "Setting not found",
                "Database error",
            ))
        }
    };
    if let Err(e) = variables::delete(ctx, &row.id).await {
        return Err(crate::blocks::crud::db_error(
            e,
            "Setting not found",
            "Database error",
        ));
    }

    audit_log(
        ctx,
        msg.user_id(),
        "variable.delete",
        &format!("variables/{key}"),
        msg.remote_addr(),
    )
    .await;
    Ok(())
}

/// Create a config variable, writing an audit-log row. Validates the value
/// with [`validate_config_value`] (SSRF for `_URL` keys, bounded lifetimes).
/// `key` must be non-empty, and must not name a key the runtime owns
/// ([`reject_runtime_owned_key`]).
///
/// `value` must also be non-empty for a key something MASKS — judged on the
/// key alone, not on the `sensitive` argument, for the reason spelled out at
/// the guard. A blank row for such a key is permanent and blocks the boot
/// seeder, which is the one shape of empty that cannot be undone.
pub(super) async fn create_variable(
    ctx: &dyn Context,
    msg: &Message,
    key: &str,
    value: &str,
    name: Option<&str>,
    description: Option<&str>,
    sensitive: bool,
) -> Result<VariableRow, OutputStream> {
    if key.is_empty() {
        return Err(err_bad_request("Key is required"));
    }
    reject_runtime_owned_key(key)?;
    let admin_id = msg.user_id().to_string();

    // A key something MASKS may not be created holding nothing. This path had
    // no empty guard at all — only [`update_variable`] did — so
    // `POST /b/admin/api/settings {"key": "…BOOTSTRAP_ADMIN_TOKEN", "value": ""}`
    // stored a blank row, and that row is both permanent and load-bearing:
    // `seed_and_load` seeds through `insert_if_absent`, which skips any key
    // that already has a row, and `delete_variable` refuses a declared
    // `WAFER_RUN_SHARED__*` row. The deployment is then wedged with no
    // bootstrap path and no way to remove the row that took it away.
    //
    // Judged on the KEY alone (`is_sensitive_key(key, 0)` — there is no stored
    // row to carry a flag), deliberately NOT on the `sensitive` argument. What
    // makes a blank row a trap rather than a nuisance is that the boot seeder
    // owns the key and `delete_variable` protects it, and both of those follow
    // from the key's declaration or its `_SECRET`/`_KEY` spelling. An ad hoc
    // key the caller merely asks to mask is deletable, and the Add Variable
    // modal checks that box by DEFAULT — gating on the argument would refuse
    // every empty variable an operator creates through the UI.
    if value.is_empty()
        && !is_clearable_provisioning_credential(ctx, key).await
        && is_sensitive_key(key, 0)
    {
        return Err(err_bad_request(&format!(
            "Cannot create {key} with an empty value"
        )));
    }

    // The per-key value rule (URL/SSRF, bounded lifetimes) on both surfaces.
    if let Err(e) = validate_config_value(key, value) {
        return Err(err_bad_request(&format!("Invalid value for {key}: {e}")));
    }

    let new = NewVariable {
        key: key.to_string(),
        value: value.to_string(),
        name: name.filter(|n| !n.is_empty()).unwrap_or(key).to_string(),
        description: description.unwrap_or_default().to_string(),
        warning: String::new(),
        sensitive,
        updated_by: msg.user_id().to_string(),
        block: variables::block_for_key(key),
    };
    let record = match variables::insert(ctx, new).await {
        Ok(row) => row,
        // `variables.key` is UNIQUE: creating a key that is already stored is a
        // 409, not a 500. See [`taken_key_or_db_error`].
        Err(e) => {
            return Err(taken_key_or_db_error(
                e,
                TakenKey::new("variable", "key", key),
                "Database error",
            ))
        }
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
    /// The masking flag, when the surface offers it. `None` leaves the stored
    /// flag alone.
    ///
    /// This is the ONLY way the flag moves downward. The boot repair pass
    /// raises but never lowers, deliberately, so a mis-flag has to be fixable
    /// by the admin who made it rather than by a boot that silently unmasks a
    /// value — which is the trade `repair_sensitive_flags_in` spells out. A
    /// key the DECLARATION or the `_SECRET`/`_KEY` suffix requires to be
    /// sensitive cannot be unflagged here either; `into_row` and the repair
    /// would only raise it again, so the write is refused rather than accepted
    /// and reverted.
    pub sensitive: Option<bool>,
}

/// The `sensitive` column of the stored row for `key`, as the `i64` flag
/// [`is_sensitive_key`] takes, or `0` when no row is stored yet.
///
/// Its own function because two of [`update_variable`]'s guards need it and
/// neither fires on an ordinary write: reading it lazily is what keeps the
/// common write path at one statement. A read failure is an error rather than
/// a guessed `0` — guessing would decide a security question by assuming the
/// answer that lets the write through.
async fn stored_sensitive_flag(ctx: &dyn Context, key: &str) -> Result<i64, OutputStream> {
    match variables::get_by_key(ctx, key).await {
        Ok(Some(row)) => Ok(i64::from(row.sensitive)),
        Ok(None) => Ok(0),
        Err(e) => Err(db_error_internal(e, "Database error")),
    }
}

/// Update a config variable identified by `key` (upsert on the `key` column),
/// writing an audit-log row. Enforces the sensitive-empty guard (a sensitive
/// value can't be cleared — see [`is_sensitive_key`]: the row's stored
/// `sensitive` flag unioned with what the key's own spelling or declaration
/// says, which is what covers Password-typed declared vars; the exception is a
/// SPENT provisioning credential, which [`is_clearable_provisioning_credential`]
/// names and which must stay clearable because nothing can delete it either)
/// and the [`validate_config_value`] rule (`_URL` SSRF, bounded lifetimes) on
/// both surfaces.
///
/// Also refuses a value that is the mask a read path emitted rather than a
/// value the caller means — see [`is_masked_submission`]. Both surfaces route
/// their writes through here, so neither can store `"********"` over a secret.
///
/// Returns the upserted row.
pub(super) async fn update_variable(
    ctx: &dyn Context,
    msg: &Message,
    key: &str,
    update: VariableUpdate<'_>,
) -> Result<VariableRow, OutputStream> {
    if key.is_empty() {
        return Err(err_bad_request("Missing setting key"));
    }
    reject_runtime_owned_key(key)?;
    let admin_id = msg.user_id().to_string();

    if let Some(value) = update.value {
        // The mask is not a value. Every read path answers a sensitive key with
        // `MASKED_VALUE`, so a client that GETs a setting and PATCHes it back —
        // the read/modify/write loop a JSON client is built around — hands
        // eight asterisks to the writer for every key it never meant to change.
        // See [`is_masked_submission`] for why this is a refusal rather than a
        // silent skip, and why it is gated on the key being masked at all.
        //
        // Checked on the same stored flag as the empty guard below, so the two
        // cannot disagree about which keys they cover. They are mutually
        // exclusive (`MASKED_VALUE` is not empty), so at most one of the two
        // row reads ever happens. The string comparison is repeated outside the
        // predicate only so that an ordinary write never pays for that read;
        // the predicate is still what decides.
        if value == MASKED_VALUE {
            let stored_flag = stored_sensitive_flag(ctx, key).await?;
            if is_masked_submission(key, stored_flag, value) {
                return Err(err_bad_request(&format!(
                    "{MASKED_VALUE} is the mask {key} reads back as, not its value: storing \
                     it would destroy the secret. Send the real value to change it, or leave \
                     the value out of the request to keep the stored one."
                )));
            }
        }
        // Prevent clearing a sensitive value (would break auth). Sensitivity
        // is the same union the read/masking paths use ([`is_sensitive_key`]):
        // the row's stored `sensitive` flag OR what the key itself says — the
        // SEC-060 `_SECRET`/`_KEY` suffix, or a declaration that is
        // `InputType::Password`/`auto_generate`, which is what names
        // `BOOTSTRAP_ADMIN_PASSWORD` and `*_TOKEN`. The stored flag still adds
        // the ad hoc rows an admin marked in the UI, about which the
        // declaration knows nothing. The row lookup only happens on the
        // empty-value path (and the masked one above); a missing row (upsert-
        // create branch) has no stored secret to wipe, so for it the key half
        // decides alone.
        // A provisioning-only credential is the exception, and it has to be,
        // because this guard and the delete path would otherwise trap it
        // between them: `delete_variable` and the Variables page's
        // `key_is_deletable` both refuse a declared `WAFER_RUN_SHARED__*` row,
        // so refusing the clear too leaves a bootstrapped deployment holding a
        // plaintext admin password it no longer needs and cannot remove by any
        // route. The guard's own reasoning — clearing it "would break auth" —
        // does not reach these: `auth::bootstrap::run` is their only reader and
        // early-returns once the `users` table is non-empty, by which point the
        // password is an argon2 hash in `local_credentials` and the token a
        // sha256 in `bootstrap_tokens`. Clearing it before provisioning is
        // equally safe: bootstrap then declines to auto-create an admin, which
        // is a documented path (`"no bootstrap admin configured"`) and
        // re-settable, not a lockout.
        //
        // NOTE the deliberate difference from the mask: an EMPTY value is a
        // refusal here but means "leave the stored value alone" on the two
        // surfaces whose widget renders a masked field blank
        // (`pages::variables::handle_update_variable` and
        // `ui::settings_form::save_settings`, which both drop it before calling
        // a writer). That is not a disagreement about the mask — the variables
        // modal sends a literal `MASKED_VALUE` straight here to be refused, and
        // `save_settings`, whose writer is `config::set` rather than this
        // function, raises the identical refusal itself. It is the widget
        // speaking: a blank masked field is the only thing a browser can post
        // for "I did not touch this", whereas an empty value arriving on the
        // JSON API was typed by a caller that had something else to say.
        if value.is_empty()
            && !is_clearable_provisioning_credential(ctx, key).await
            && is_sensitive_key(key, stored_sensitive_flag(ctx, key).await?)
        {
            return Err(err_bad_request(&format!(
                "Cannot set {key} to an empty value"
            )));
        }
        // The per-key value rule (URL/SSRF, bounded lifetimes) on both surfaces.
        if let Err(e) = validate_config_value(key, value) {
            return Err(err_bad_request(&format!("Invalid value for {key}: {e}")));
        }
    } else if variables::get_by_key(ctx, key)
        .await
        .map_err(|e| db_error_internal(e, "Database error"))?
        .is_none()
    {
        // No value supplied AND no row stored. This function upserts, so the
        // write below would take the create branch and store `value: ""` —
        // sailing past the empty guard above, which only runs when a value was
        // supplied at all. While `value` was a required request field that was
        // unreachable; making it optional (so a caller can change the
        // `sensitive` flag of a key whose value it cannot read) opened it, and
        // the row it produces is the permanent, undeletable, seeder-blocking
        // blank `create_variable`'s guard above describes.
        //
        // Refused for every key, not just the masked ones: a create that
        // carries no value has nothing to create the row FROM. "Update the
        // other fields of a variable that exists" and "bring a variable into
        // existence" are different requests, and only the second needs a value
        // — so the second is the one that has to supply it.
        return Err(err_bad_request(&format!(
            "Cannot create {key} without a value: a request that changes only other fields \
             updates a variable that already exists, it does not create one."
        )));
    }

    // A PUT to a not-yet-present key takes `upsert_by_key`'s create branch,
    // which derives the row's `block` and synthesises its id and timestamps.
    //
    // The patch leaves `sensitive` unset, and both halves of what settles it
    // live elsewhere — neither of them here, which is why this says which:
    //
    //   * a DECLARED key (or a `_SECRET`/`_KEY` suffix) is raised by
    //     `NewVariable::into_row`, the funnel every row creation passes
    //     through. That is what stopped a PUT creating
    //     `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_TOKEN` unflagged.
    //   * an UNDECLARED ad hoc key is not something that funnel can speak to —
    //     it raises from the declaration, and there is none — so
    //     `VariablePatch::into_new` defaults it through
    //     `config_vars::is_sensitive_by_default_when_created`. Without that,
    //     `PATCH /b/admin/api/settings/MY_SERVICE_TOKEN` on a key with no row
    //     stored it unflagged and the next GET published it, while the same
    //     key through POST was protected by `handle_create`'s "absent means
    //     sensitive" rule. The two surfaces now agree.
    // Refuse an unflag the storage rule would immediately undo: `into_row` and
    // the boot repair pass both raise from the declaration and the
    // `_SECRET`/`_KEY` suffix, so accepting it would report success for a
    // change the next write or boot reverses.
    if update.sensitive == Some(false) && crate::config_vars::is_sensitive_for_storage(key) {
        return Err(err_bad_request(&format!(
            "Cannot un-mark {key} as sensitive: its declaration (or its \
             _SECRET/_KEY suffix) requires it"
        )));
    }
    let patch = VariablePatch {
        value: update.value.map(str::to_string),
        description: update.description.map(str::to_string),
        sensitive: update.sensitive,
        updated_by: Some(msg.user_id().to_string()),
        ..Default::default()
    };
    let record = match variables::upsert_by_key(ctx, key, patch).await {
        Ok(row) => row,
        // `db_error_internal`, not a bare `err_internal`: an upsert names its
        // own table and no row of the caller's, so a `NotFound` is a 500 — but a WRAP
        // refusal is a 403 and a quota a 429, and forwarding those as 500 is
        // the drift `tests/error_door.rs` exists to stop.
        Err(e) => return Err(db_error_internal(e, "Database error")),
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
    use wafer_block::db::{Filter, FilterOp};

    use super::*;
    use crate::config_vars::{APP_NAME_KEY, EMBEDDED_SCRIPTS_KEY};
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
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
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
        crate::db_read::list_every(ctx, super::super::logs::AUDIT_LOGS_TABLE, filters)
            .await
            .map(|r| r.len())
            .unwrap_or(0)
    }

    /// Deletion is audited like creation and update. Before the two surfaces
    /// shared this helper, the JSON delete path wrote no audit row at all.
    #[tokio::test]
    async fn deleting_a_variable_removes_the_row_and_audits() {
        let ctx = admin_ctx().await;
        let msg = admin_msg("create", "/admin/settings");
        create_variable(&ctx, &msg, "LEGACY_THING", "v", None, None, false)
            .await
            .map_err(|_| "seed")
            .expect("seed the variable");

        delete_variable(&ctx, &msg, "LEGACY_THING")
            .await
            .map_err(|_| "delete")
            .expect("delete must succeed");

        assert!(
            variables::get_by_key(&ctx, "LEGACY_THING")
                .await
                .expect("read back")
                .is_none(),
            "the row must be gone",
        );
        assert_eq!(audit_count(&ctx, "variable.delete").await, 1);
    }

    /// The JWT signing secret is not deletable.
    ///
    /// It reaches the Variables page's "unowned" table (the auth block never
    /// declares it as a `ConfigVar`), so before this guard it sat under a
    /// trash icon. Deleting it looks harmless until the next boot mints a
    /// different secret and every session and CSRF token stops verifying.
    #[tokio::test]
    async fn deleting_the_jwt_signing_secret_is_refused() {
        let ctx = admin_ctx().await;
        let msg = admin_msg("create", "/admin/settings");
        let key = crate::blocks::auth::JWT_SECRET_KEY;
        create_variable(&ctx, &msg, key, "s3cret", None, None, false)
            .await
            .map_err(|_| "seed")
            .expect("seed the secret");

        assert!(
            delete_variable(&ctx, &msg, key).await.is_err(),
            "the signing secret must not be deletable",
        );
        assert!(
            variables::get_by_key(&ctx, key)
                .await
                .expect("read back")
                .is_some(),
            "the refused row must still be there",
        );
        assert_eq!(audit_count(&ctx, "variable.delete").await, 0);
    }

    /// A DECLARED shared var is refused — it is re-seeded on the next boot, so
    /// deleting it is a no-op that looks like a change.
    #[tokio::test]
    async fn deleting_a_declared_shared_variable_is_refused() {
        let ctx = admin_ctx().await;
        let msg = admin_msg("create", "/admin/settings");
        let key = APP_NAME_KEY;
        create_variable(&ctx, &msg, key, "Shop", None, None, false)
            .await
            .map_err(|_| "seed")
            .expect("seed the shared var");

        assert!(delete_variable(&ctx, &msg, key).await.is_err());
        assert!(variables::get_by_key(&ctx, key)
            .await
            .expect("read back")
            .is_some());
    }

    /// A STALE one is not. Once a key is dropped from `shared_config_vars()`
    /// nothing re-seeds it, so the "it comes back anyway" reasoning stops
    /// applying and the row is dead data — precisely what an operator needs to
    /// be able to remove.
    #[tokio::test]
    async fn deleting_an_undeclared_shared_variable_is_allowed() {
        let ctx = admin_ctx().await;
        let msg = admin_msg("create", "/admin/settings");
        const RETIRED: &str = "WAFER_RUN_SHARED__RETIRED_SETTING";
        assert!(
            !crate::config_vars::shared_config_vars()
                .iter()
                .any(|v| v.key == RETIRED),
            "fixture must name a key the shared list does not declare",
        );
        create_variable(&ctx, &msg, RETIRED, "x", None, None, false)
            .await
            .map_err(|_| "seed")
            .expect("seed the stale row");

        delete_variable(&ctx, &msg, RETIRED)
            .await
            .map_err(|_| "delete")
            .expect("a stale shared row must be removable");
        assert!(variables::get_by_key(&ctx, RETIRED)
            .await
            .expect("read back")
            .is_none());
    }

    /// Creating a variable whose key is already taken is a **409**, not a 500.
    ///
    /// The `key` column is `UNIQUE`, so the second insert is refused by the
    /// database, as `ErrorCode::AlreadyExists`. Forwarded as
    /// `err_internal("Database error", …)` that is a `500 Internal server
    /// error (ref: …)`, which tells an operator their request broke the server
    /// when in fact the server is fine and the request named a key that
    /// exists. The row must also be left exactly as it was.
    #[tokio::test]
    async fn creating_a_variable_whose_key_is_taken_is_a_conflict() {
        let ctx = admin_ctx().await;
        let msg = admin_msg("create", "/admin/settings");
        expect_ok(create_variable(&ctx, &msg, "SITE_NAME", "Acme", None, None, false).await);

        let Err(out) = create_variable(&ctx, &msg, "SITE_NAME", "Other", None, None, false).await
        else {
            panic!("the duplicate key must not be created");
        };
        assert_eq!(
            crate::test_support::output_http_status(out).await,
            409,
            "a taken key is a conflict, not an internal error",
        );

        let row = variables::get_by_key(&ctx, "SITE_NAME")
            .await
            .expect("read back")
            .expect("the first row must still be there");
        assert_eq!(row.value, "Acme", "the refused write must change nothing");
        assert_eq!(
            audit_count(&ctx, "variable.create").await,
            1,
            "a refused create is not an audited create",
        );
    }

    /// A create whose row LANDED is never reported as a conflict, however thin
    /// the backend's echo is.
    ///
    /// `DatabaseService::create` may answer with the row it stored or with an
    /// acknowledgement. `variables::insert` used to decode that echo and return
    /// the decode failure as an error — and a failed insert was then
    /// classified by re-reading the key, which found the row this very request
    /// had just written. The admin was told the key already existed, the
    /// `variable.create` audit row was skipped, and the untracked row made
    /// every retry conflict forever. The write succeeded, so the answer has to
    /// say so.
    #[tokio::test]
    async fn a_create_whose_echo_is_empty_is_a_create_not_a_conflict() {
        let ctx = crate::test_support::EcholessWriteContext::new(admin_ctx().await);
        let msg = admin_msg("create", "/admin/settings");

        let row =
            expect_ok(create_variable(&ctx, &msg, "SITE_NAME", "Acme", None, None, false).await);
        assert_eq!(
            row.key, "SITE_NAME",
            "the row as written is what the caller gets back",
        );
        assert_eq!(row.value, "Acme");

        assert_eq!(
            audit_count(&ctx, "variable.create").await,
            1,
            "a create that landed must not go untracked",
        );
        let stored = variables::get_by_key(&ctx, "SITE_NAME")
            .await
            .expect("read back")
            .expect("the row really is in the table");
        assert_eq!(stored.value, "Acme");
    }

    /// An UPDATE whose row landed is audited, however thin the backend's echo
    /// is.
    ///
    /// The other half of the same rule, with a different consequence.
    /// `update_variable` never answers a conflict, so a decode failure after
    /// a committed `db::update` was "only" a 500 — but it returned BEFORE
    /// `audit_log`, so the edit happened and nothing recorded it. An audit
    /// trail that is missing a change it should contain cannot be told apart
    /// from the change never having been made, which is worse than either the
    /// 500 or the false 409.
    #[tokio::test]
    async fn an_update_whose_echo_is_empty_is_audited_and_returns_the_new_value() {
        let seeded = admin_ctx().await;
        let msg = admin_msg("update", "/admin/settings");
        expect_ok(create_variable(&seeded, &msg, "SITE_NAME", "Acme", None, None, false).await);

        let ctx = crate::test_support::EcholessWriteContext::new(seeded);
        let row = expect_ok(
            update_variable(
                &ctx,
                &msg,
                "SITE_NAME",
                VariableUpdate {
                    value: Some("Acme Two"),
                    description: None,
                    sensitive: None,
                },
            )
            .await,
        );

        assert_eq!(
            row.value, "Acme Two",
            "the columns just written win over the row that was read",
        );
        assert_eq!(
            row.key, "SITE_NAME",
            "and every column the update did not touch is carried over",
        );
        assert_eq!(
            audit_count(&ctx, "variable.update").await,
            1,
            "an edit that landed must not go unrecorded",
        );
        assert_eq!(
            variables::get_by_key(&ctx, "SITE_NAME")
                .await
                .expect("read back")
                .expect("still there")
                .value,
            "Acme Two",
        );
    }

    /// A PARTIAL echo does not report a still-sensitive variable as no longer
    /// sensitive.
    ///
    /// This is the user-visible half of the merge rule. `from_record` insists
    /// on `key` alone, so a backend echoing `key` and `value` decodes fine —
    /// and the row that came back had `sensitive: false` for a variable that is
    /// still flagged. Both admin surfaces mask on that flag, so the refusal
    /// path is the mild version; the row published back to the caller after an
    /// edit was the loud one.
    #[tokio::test]
    async fn an_update_with_a_partial_echo_keeps_the_sensitive_flag() {
        let seeded = admin_ctx().await;
        let msg = admin_msg("update", "/admin/settings");
        expect_ok(
            create_variable(
                &seeded,
                &msg,
                "MAILER_TOKEN",
                "tok-1",
                Some("Mailer token"),
                None,
                true,
            )
            .await,
        );

        let ctx = crate::test_support::EcholessWriteContext::new(seeded)
            .keeping_columns(&["key", "value"]);
        let row = expect_ok(
            update_variable(
                &ctx,
                &msg,
                "MAILER_TOKEN",
                VariableUpdate {
                    value: Some("tok-2"),
                    description: None,
                    sensitive: None,
                },
            )
            .await,
        );

        assert_eq!(row.value, "tok-2", "the column the update wrote");
        assert!(
            row.sensitive,
            "a column the echo left out must come from the row, not from a default",
        );
        assert_eq!(row.name, "Mailer token");
        assert!(!row.created_at.is_empty());
    }

    /// The same fact for roles: `roles.name` is UNIQUE too, and the identical
    /// `err_internal` tail two functions above `create_variable` answered the
    /// identical 500. Fixed together so the two copies cannot drift again.
    #[tokio::test]
    async fn creating_a_role_whose_name_is_taken_is_a_conflict() {
        let ctx = admin_ctx().await;
        let msg = admin_msg("create", "/admin/users");
        expect_ok(create_role(&ctx, &msg, "editor", Some("first"), None).await);

        let Err(out) = create_role(&ctx, &msg, "editor", Some("second"), None).await else {
            panic!("the duplicate role name must not be created");
        };
        assert_eq!(
            crate::test_support::output_http_status(out).await,
            409,
            "a taken role name is a conflict, not an internal error",
        );
        assert_eq!(
            audit_count(&ctx, "role.create").await,
            1,
            "a refused create is not an audited create",
        );
    }

    /// A create that fails for a reason which is NOT a name collision keeps the
    /// 500 — only the write's own `AlreadyExists` is a conflict, so the
    /// classification cannot become "every failed insert is a conflict".
    #[tokio::test]
    async fn a_create_that_fails_without_a_collision_is_still_internal() {
        let ctx = admin_ctx().await.break_writes();
        let msg = admin_msg("create", "/admin/settings");

        let Err(out) = create_variable(&ctx, &msg, "SITE_NAME", "Acme", None, None, false).await
        else {
            panic!("the write is broken, so the create must fail");
        };
        assert_eq!(
            crate::test_support::output_http_status(out).await,
            500,
            "a failure other than AlreadyExists is the write's own fault",
        );
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
                    sensitive: None,
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
                    sensitive: None,
                },
            )
            .await,
        );
        assert_eq!(
            record.key, "NEW_SITE_TAGLINE",
            "the created row must persist its key column"
        );

        // The row is now findable by `key` (proves the NOT NULL row landed).
        let found = variables::get_by_key(&ctx, "NEW_SITE_TAGLINE")
            .await
            .expect("get variable")
            .expect("the upserted variable is findable by key");
        assert_eq!(found.value, "Hello");

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
                    sensitive: None,
                },
            )
            .await,
        );
        let rows: Vec<_> = variables::list_all(&ctx)
            .await
            .expect("list variables")
            .into_iter()
            .filter(|row| row.key == "NEW_SITE_TAGLINE")
            .collect();
        assert_eq!(rows.len(), 1, "update must not create a second row");
        assert_eq!(rows[0].value, "Goodbye");
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
                sensitive: None,
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
                sensitive: None,
            },
        )
        .await
        .is_err());
    }

    /// A bootstrapped deployment must be able to CLEAR the plaintext admin
    /// credential it no longer needs, and logging in must keep working after
    /// it does.
    ///
    /// Flagging `BOOTSTRAP_ADMIN_PASSWORD` sensitive (it is declared
    /// `InputType::Password`) brought it under the sensitive-empty guard, while
    /// `delete_variable` and the Variables page's `key_is_deletable` already
    /// refuse to delete a declared `WAFER_RUN_SHARED__*` row. Together those
    /// left the credential unremovable by any route — the masking and the
    /// removal path fighting each other over the very row
    /// `repair_sensitive_flags` exists for.
    ///
    /// Drives the real chain: the env seeder writes the row, `bootstrap::run`
    /// consumes it, `update_variable` clears it, and the real login handler
    /// proves auth is untouched — because the password's surviving form is the
    /// argon2 hash in `local_credentials`, not this row.
    #[tokio::test]
    async fn a_bootstrapped_admin_credential_can_be_cleared_and_login_still_works() {
        use crate::blocks::auth::config::{
            BOOTSTRAP_ADMIN_EMAIL_KEY, BOOTSTRAP_ADMIN_PASSWORD_KEY,
        };

        const EMAIL: &str = "admin@example.com";
        const PASSWORD: &str = "correct-horse-battery";

        // Each step runs in the frame production runs it in: auth's Init for
        // the bootstrap, the admin block for the settings edit, auth-ui for
        // the login. `ctx` is the test's own, for staging and asserting.
        let ctx = TestContext::with_auth_and_crypto().await.fixture();
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");

        // The operator's `.env`, through the production seeder.
        ctx.seed_env_vars(&[
            (BOOTSTRAP_ADMIN_EMAIL_KEY, EMAIL),
            (BOOTSTRAP_ADMIN_PASSWORD_KEY, PASSWORD),
        ])
        .await;
        assert!(
            variables::get_by_key(&ctx, BOOTSTRAP_ADMIN_PASSWORD_KEY)
                .await
                .expect("get")
                .expect("row")
                .sensitive,
            "the credential is stored flagged — that is what brings it under the guard"
        );

        // First run: the credential is consumed into `local_credentials`.
        let cfg = crate::blocks::auth::config::AuthConfig::from_env_for_test(&[
            (BOOTSTRAP_ADMIN_EMAIL_KEY, EMAIL),
            (BOOTSTRAP_ADMIN_PASSWORD_KEY, PASSWORD),
        ]);
        crate::blocks::auth::bootstrap::run(&ctx.clone().running_as("wafer-run/auth"), &cfg)
            .await
            .expect("bootstrap the admin user");

        // The operator removes the export and clears the stored plaintext
        // through the admin API. This 400'd before the exemption.
        let msg = admin_msg("update", "/admin/settings");
        expect_ok(
            update_variable(
                &ctx.clone().running_as(crate::blocks::admin::ADMIN_BLOCK_ID),
                &msg,
                BOOTSTRAP_ADMIN_PASSWORD_KEY,
                VariableUpdate {
                    value: Some(""),
                    description: None,
                    sensitive: None,
                },
            )
            .await,
        );
        assert_eq!(
            variables::get_by_key(&ctx, BOOTSTRAP_ADMIN_PASSWORD_KEY)
                .await
                .expect("get")
                .expect("the row survives, emptied")
                .value,
            "",
            "the plaintext credential must actually be gone"
        );

        // And the bootstrapped admin can still log in.
        let body = serde_json::json!({"email": EMAIL, "password": PASSWORD}).to_string();
        let resp = crate::test_support::output_json(
            crate::blocks::auth_ui::api::login::handle(
                &ctx.clone()
                    .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID),
                wafer_run::InputStream::from_bytes(body.into_bytes()),
            )
            .await,
        )
        .await;
        assert_eq!(
            resp["user"]["email"],
            serde_json::json!(EMAIL),
            "clearing the spent provisioning copy must not affect authentication: {resp}"
        );
    }

    /// A session lifetime already stored past its bound fails the login with
    /// a 500 instead of taking the server down.
    ///
    /// The lifetime is added to the current time for the refresh token's
    /// expiry, and `100000000` days reaches past the last date chrono can
    /// represent, where `DateTime + Duration` panics — and a release build
    /// aborts on a panic, so every login killed the native process. The write
    /// surfaces refuse such a value now, so the row is staged directly, as an
    /// older build or the process environment could have left it. Drives the
    /// real login handler through the production config block.
    #[tokio::test]
    async fn an_out_of_range_stored_session_lifetime_fails_login_with_a_500_not_a_crash() {
        use crate::blocks::auth::config::SESSION_LIFETIME_DAYS_KEY;

        const EMAIL: &str = "lifetime@example.com";
        const PASSWORD: &str = "correct-horse-battery";

        let ctx = TestContext::with_auth_and_crypto().await;
        crate::blocks::auth::repo::local_credentials::insert(
            &ctx,
            &crate::test_support::seed_user(EMAIL).insert(&ctx).await.id,
            &wafer_core::clients::crypto::hash(&ctx, PASSWORD)
                .await
                .expect("hash the password"),
            false,
        )
        .await
        .expect("store the credential");
        variables::seed_row_with_flag(&ctx, SESSION_LIFETIME_DAYS_KEY, "100000000", 0).await;
        // The raw fixture insert skips the repo's generation bump; without it
        // a config snapshot warmed earlier would keep serving the default.
        crate::config_generation::note_config_write();

        let body = serde_json::json!({"email": EMAIL, "password": PASSWORD}).to_string();
        let status = crate::test_support::output_http_status(
            crate::blocks::auth_ui::api::login::handle(
                &ctx,
                wafer_run::InputStream::from_bytes(body.into_bytes()),
            )
            .await,
        )
        .await;
        assert_eq!(
            status, 500,
            "a misconfigured session lifetime must refuse the login, not issue tokens"
        );
    }

    /// A refresh refused for a misconfigured session lifetime leaves the
    /// presented refresh token live.
    ///
    /// The refresh handler claims the token row (compare-and-set revoke)
    /// before it mints the successor. Reading the lifetime only inside
    /// issuance meant the claim had already spent the token when the read
    /// failed, so every session died at its next refresh even after the
    /// operator fixed the value. Drives the real login and refresh handlers:
    /// the same token must refresh once the value is corrected.
    #[tokio::test]
    async fn a_refresh_refused_for_a_bad_session_lifetime_keeps_the_token_live() {
        use crate::blocks::auth::config::SESSION_LIFETIME_DAYS_KEY;

        const EMAIL: &str = "refresh-lifetime@example.com";
        const PASSWORD: &str = "correct-horse-battery";

        let ctx = TestContext::with_auth_and_crypto().await;
        crate::blocks::auth::repo::local_credentials::insert(
            &ctx,
            &crate::test_support::seed_user(EMAIL).insert(&ctx).await.id,
            &wafer_core::clients::crypto::hash(&ctx, PASSWORD)
                .await
                .expect("hash the password"),
            false,
        )
        .await
        .expect("store the credential");

        let body = serde_json::json!({"email": EMAIL, "password": PASSWORD}).to_string();
        let login = crate::test_support::output_json(
            crate::blocks::auth_ui::api::login::handle(
                &ctx,
                wafer_run::InputStream::from_bytes(body.into_bytes()),
            )
            .await,
        )
        .await;
        let refresh_token = login["refresh_token"]
            .as_str()
            .expect("login returns a refresh token")
            .to_string();
        let refresh = || async {
            let body = serde_json::json!({ "refresh_token": refresh_token }).to_string();
            crate::test_support::output_http_status(
                crate::blocks::auth_ui::api::refresh::handle(
                    &ctx,
                    wafer_run::InputStream::from_bytes(body.into_bytes()),
                )
                .await,
            )
            .await
        };

        variables::seed_row_with_flag(&ctx, SESSION_LIFETIME_DAYS_KEY, "100000000", 0).await;
        // The raw fixture insert skips the repo's generation bump.
        crate::config_generation::note_config_write();
        assert_eq!(
            refresh().await,
            500,
            "the misconfigured lifetime refuses the refresh"
        );

        // The operator corrects it through the admin API.
        expect_ok(
            update_variable(
                &ctx.fixture()
                    .running_as(crate::blocks::admin::ADMIN_BLOCK_ID),
                &admin_msg("update", "/admin/settings"),
                SESSION_LIFETIME_DAYS_KEY,
                VariableUpdate {
                    value: Some("7"),
                    description: None,
                    sensitive: None,
                },
            )
            .await,
        );
        assert_eq!(
            refresh().await,
            200,
            "the refused refresh must not have spent the token"
        );
    }

    /// A PUT that CREATES an undeclared ad hoc key must protect it the same
    /// way a POST does.
    ///
    /// `update_variable`'s patch never sets `sensitive`, so the create branch
    /// runs `VariablePatch::into_new`. That defaulted to `false`, and
    /// `NewVariable::into_row`'s funnel cannot save it — the funnel raises from
    /// the declaration or the `_SECRET`/`_KEY` suffix, and an ad hoc key is
    /// neither. So `MY_SERVICE_TOKEN` was stored unflagged and published by the
    /// next GET, while the identical key through POST was flagged by
    /// `handle_create`'s "absent means sensitive" rule.
    #[tokio::test]
    async fn a_put_created_ad_hoc_key_is_protected_like_a_post_created_one() {
        let ctx = admin_ctx().await;
        let msg = admin_msg("update", "/admin/settings");
        const KEY: &str = "MY_SERVICE_TOKEN";
        assert!(
            !crate::config_vars::is_declared_key(KEY)
                && !crate::config_vars::has_sensitive_suffix(KEY),
            "the point of this test is a key neither the declaration nor the suffix catches"
        );

        expect_ok(
            update_variable(
                &ctx,
                &msg,
                KEY,
                VariableUpdate {
                    value: Some("tok_live_abc"),
                    description: None,
                    sensitive: None,
                },
            )
            .await,
        );
        assert!(
            variables::get_by_key(&ctx, KEY)
                .await
                .expect("get")
                .expect("row")
                .sensitive,
            "a PUT-created ad hoc key must be stored sensitive, as POST would"
        );

        // A declared var is NOT swept up by that default: the build knows what
        // it is, so the declaration answers.
        expect_ok(
            update_variable(
                &ctx,
                &msg,
                APP_NAME_KEY,
                VariableUpdate {
                    value: Some("Acme"),
                    description: None,
                    sensitive: None,
                },
            )
            .await,
        );
        assert!(
            !variables::get_by_key(&ctx, APP_NAME_KEY)
                .await
                .expect("get")
                .expect("row")
                .sensitive,
            "a declared non-secret var must not be masked just because it was PUT"
        );
    }

    /// An admin can clear a flag they set by mistake, and cannot clear one the
    /// storage rule requires.
    ///
    /// This is the recovery route the boot repair pass deliberately does NOT
    /// provide: it raises only, so nothing un-masks a value behind an
    /// operator's back. That only works if the person who mis-flagged a row
    /// can fix it, which before this needed delete-and-recreate —
    /// `VariableUpdate` carried no `sensitive`, the edit modal rendered only
    /// value and description, and `create_variable` 409s on an existing key.
    #[tokio::test]
    async fn an_admin_can_clear_a_mistaken_flag_but_not_a_required_one() {
        let ctx = admin_ctx().await;
        let msg = admin_msg("update", "/admin/settings");

        // Mis-flagged by the Add Variable modal's default tick.
        let key = EMBEDDED_SCRIPTS_KEY;
        expect_ok(create_variable(&ctx, &msg, key, "/analytics.js", None, None, true).await);
        expect_ok(
            update_variable(
                &ctx,
                &msg,
                key,
                VariableUpdate {
                    value: None,
                    description: None,
                    sensitive: Some(false),
                },
            )
            .await,
        );
        assert!(
            !variables::get_by_key(&ctx, key)
                .await
                .expect("get")
                .expect("row")
                .sensitive,
            "an admin must be able to undo their own mis-flag"
        );

        // And a key the declaration requires cannot be unflagged, because the
        // funnel and the repair pass would only raise it again.
        let required = crate::blocks::auth::config::BOOTSTRAP_ADMIN_PASSWORD_KEY;
        expect_ok(create_variable(&ctx, &msg, required, "hunter2", None, None, true).await);
        assert!(
            update_variable(
                &ctx,
                &msg,
                required,
                VariableUpdate {
                    value: None,
                    description: None,
                    sensitive: Some(false),
                },
            )
            .await
            .is_err(),
            "accepting this would report success for a change the next boot reverses"
        );
    }

    /// Review finding 5, as a test: clearing the spent bootstrap password must
    /// STAY cleared across the next boot, with the export still present.
    ///
    /// Under env-wins it did not — the next `seed_and_load` re-applied the
    /// still-present export and handed the plaintext credential back.
    ///
    /// Both provenances, because the two surfaces that can clear it leave
    /// different traces and only one of them is new. `update_variable` (this
    /// module) has always stamped `updated_by`, so a row it cleared is pinned as
    /// an admin edit. The auth-ui settings form clears the same key through
    /// `ui::settings_form` -> `config::set` -> `CONFIG_SET`, which reached
    /// `variables::set` and stamped NOTHING before this release — so on an
    /// upgrading deployment the cleared row is indistinguishable from a seeded
    /// one, and it is the upgrade transition, not the stamp, that has to keep
    /// it cleared. A test that drove only the stamping path would pass with the
    /// transition deleted.
    #[tokio::test]
    async fn a_cleared_bootstrap_password_stays_cleared_across_the_next_boot() {
        use crate::blocks::auth::config::BOOTSTRAP_ADMIN_PASSWORD_KEY as KEY;

        // 1. Cleared through this module's API, which stamps.
        let ctx = admin_ctx().await;
        let msg = admin_msg("update", "/admin/settings");
        ctx.seed_env_vars(&[(KEY, "hunter2")]).await;
        expect_ok(
            update_variable(
                &ctx,
                &msg,
                KEY,
                VariableUpdate {
                    value: Some(""),
                    description: None,
                    sensitive: None,
                },
            )
            .await,
        );
        // The operator has not removed the export yet — the realistic state.
        ctx.seed_env_vars(&[(KEY, "hunter2")]).await;
        assert_eq!(
            variables::get_by_key(&ctx, KEY)
                .await
                .expect("get")
                .expect("row")
                .value,
            "",
            "a credential an admin cleared must not come back on the next boot"
        );

        // 2. Cleared through the settings form BEFORE this release, which left
        //    no marker at all. Nothing has booted this database since, so the
        //    upgrade transition has not run either.
        let ctx = admin_ctx().await;
        variables::seed_row_with_owner(&ctx, KEY, "", "").await;
        assert!(
            !variables::is_pinned(
                &variables::get_by_key(&ctx, KEY)
                    .await
                    .expect("get")
                    .expect("row")
            ),
            "the pre-upgrade row carries no provenance — that is the case under test"
        );

        ctx.seed_env_vars(&[(KEY, "hunter2")]).await;
        assert_eq!(
            variables::get_by_key(&ctx, KEY)
                .await
                .expect("get")
                .expect("row")
                .value,
            "",
            "the upgrade boot must not hand a spent plaintext credential back"
        );
    }

    /// Try to clear the bootstrap token through the real admin update path.
    async fn try_clear_token(ctx: &TestContext, msg: &Message) -> bool {
        update_variable(
            ctx,
            msg,
            crate::blocks::auth::config::BOOTSTRAP_ADMIN_TOKEN_KEY,
            VariableUpdate {
                value: Some(""),
                description: None,
                sensitive: None,
            },
        )
        .await
        .is_ok()
    }

    /// The token exemption turns on whether the token has been REDEEMED.
    ///
    /// Unredeemed (`users` empty) it is a live credential: every boot re-reads
    /// the plaintext and mints a fresh 24h `bootstrap_tokens` row, so clearing
    /// it strands the deployment with no admin path. Redeemed, it is inert —
    /// and since `into_row` now flags it and `delete_variable` refuses a
    /// declared `WAFER_RUN_SHARED__*` row, refusing the clear too would leave
    /// a plaintext credential removable by no route at all, which is the exact
    /// trap the password exemption exists to avoid.
    #[tokio::test]
    async fn the_bootstrap_token_is_clearable_only_once_it_has_been_redeemed() {
        use crate::blocks::auth::config::BOOTSTRAP_ADMIN_TOKEN_KEY as KEY;

        // Unredeemed: no users yet.
        let ctx = TestContext::with_admin()
            .await
            .with_auth_added()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        let msg = admin_msg("update", "/admin/settings");
        expect_ok(create_variable(&ctx, &msg, KEY, "tok", None, None, true).await);
        assert!(
            !try_clear_token(&ctx, &msg).await,
            "an unredeemed token is live; clearing it would be a lockout"
        );

        // Redeemed: an admin user exists.
        let ctx = TestContext::with_admin()
            .await
            .with_auth_added()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        ctx.seed_auth_user("admin_1").await;
        let msg = admin_msg("update", "/admin/settings");
        expect_ok(create_variable(&ctx, &msg, KEY, "tok", None, None, true).await);
        assert!(
            try_clear_token(&ctx, &msg).await,
            "a redeemed token is inert and must be removable"
        );
    }

    /// A pinned key must have a way out, and the two routes the WARN used to
    /// name are not it.
    ///
    /// `delete_variable` refuses every declared `WAFER_RUN_SHARED__*` row —
    /// which is every key that reaches the env loop — and `update_variable`
    /// stamps `updated_by` on every write, so clearing the value re-pins the
    /// row it was meant to release. Both are asserted here, so the message and
    /// the code cannot drift apart again.
    #[tokio::test]
    async fn reset_to_environment_is_the_only_route_out_of_a_pinned_key() {
        let ctx = admin_ctx().await;
        let msg = admin_msg("update", "/admin/settings");
        let key = APP_NAME_KEY;

        expect_ok(create_variable(&ctx, &msg, key, "AdminChoice", None, None, false).await);
        assert!(
            variables::is_pinned(
                &variables::get_by_key(&ctx, key)
                    .await
                    .expect("get")
                    .expect("row")
            ),
            "an admin create pins the key"
        );

        // Route one, as the old message advised: refused.
        assert!(
            delete_variable(&ctx, &msg, key).await.is_err(),
            "a declared shared var cannot be deleted, so 'delete the row' was bad advice"
        );

        // Route two, as the old message advised: leaves it pinned.
        expect_ok(
            update_variable(
                &ctx,
                &msg,
                key,
                VariableUpdate {
                    value: Some("something else"),
                    description: None,
                    sensitive: None,
                },
            )
            .await,
        );
        assert!(
            variables::is_pinned(
                &variables::get_by_key(&ctx, key)
                    .await
                    .expect("get")
                    .expect("row")
            ),
            "an update re-stamps ownership, so clearing could never release the key"
        );

        // The route that exists.
        expect_ok(reset_variable_to_environment(&ctx, &msg, key).await);
        let row = variables::get_by_key(&ctx, key)
            .await
            .expect("get")
            .expect("row");
        assert!(!variables::is_pinned(&row), "the key is released");
        assert_eq!(
            row.value, "something else",
            "and the value stays until a boot re-seeds it"
        );
        assert_eq!(audit_count(&ctx, "variable.reset_to_environment").await, 1);
    }

    /// The bootstrap TOKEN must stay unclearable, unlike the password.
    ///
    /// The difference is which branch of `auth::bootstrap::run` populates
    /// `wafer_run__auth__users`: the email+password branch inserts a user, so
    /// from the next boot `run` early-returns and the password is spent. The
    /// token branch inserts only into `bootstrap_tokens` and creates NO user,
    /// so `users` stays empty, `run` re-runs on every boot (it is called from
    /// `AuthServiceImpl::init`), and the plaintext row keeps minting fresh 24h
    /// tokens. Clearing it on a deployment with no admin yet lets the
    /// outstanding token expire with nothing to regenerate it and no admin path
    /// left — and Cloudflare has no process environment to re-seed from.
    #[tokio::test]
    async fn the_bootstrap_token_cannot_be_cleared_but_the_password_can() {
        use crate::blocks::auth::config::{
            BOOTSTRAP_ADMIN_PASSWORD_KEY, BOOTSTRAP_ADMIN_TOKEN_KEY,
        };

        let ctx = admin_ctx().await;
        let msg = admin_msg("update", "/admin/settings");
        let clear = |key: &'static str| {
            let ctx = &ctx;
            let msg = &msg;
            async move {
                update_variable(
                    ctx,
                    msg,
                    key,
                    VariableUpdate {
                        value: Some(""),
                        description: None,
                        sensitive: None,
                    },
                )
                .await
            }
        };

        expect_ok(
            create_variable(
                &ctx,
                &msg,
                BOOTSTRAP_ADMIN_TOKEN_KEY,
                "tok",
                None,
                None,
                true,
            )
            .await,
        );
        assert!(
            clear(BOOTSTRAP_ADMIN_TOKEN_KEY).await.is_err(),
            "the token is re-read on every boot and reissued; clearing it is a lockout"
        );

        expect_ok(
            create_variable(
                &ctx,
                &msg,
                BOOTSTRAP_ADMIN_PASSWORD_KEY,
                "hunter2",
                None,
                None,
                true,
            )
            .await,
        );
        expect_ok(clear(BOOTSTRAP_ADMIN_PASSWORD_KEY).await);
    }

    /// The exemption is exactly one key wide. A live secret still cannot be
    /// cleared, which is what the guard is for.
    #[tokio::test]
    async fn clearing_a_live_secret_is_still_refused() {
        let ctx = admin_ctx().await;
        let msg = admin_msg("update", "/admin/settings");
        expect_ok(
            create_variable(
                &ctx,
                &msg,
                "X__Y__STRIPE_SECRET",
                "sk_live",
                None,
                None,
                true,
            )
            .await,
        );
        assert!(
            update_variable(
                &ctx,
                &msg,
                "X__Y__STRIPE_SECRET",
                VariableUpdate {
                    value: Some(""),
                    description: None,
                    sensitive: None,
                },
            )
            .await
            .is_err(),
            "a live secret must still be unclearable"
        );
        assert!(
            !crate::config_vars::is_provisioning_only_key(
                crate::blocks::auth::config::BOOTSTRAP_ADMIN_EMAIL_KEY
            ),
            "the bootstrap EMAIL stays live after provisioning (TokenGrant::resolve reads it \
             on every token mint), so it is not provisioning-only"
        );
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
                sensitive: None,
            },
        )
        .await
        .is_err());

        // ...leaving the stored value untouched.
        let row = variables::get_by_key(&ctx, "BOOTSTRAP_ADMIN_PASSWORD")
            .await
            .expect("get variable")
            .expect("seeded row still present");
        assert_eq!(
            row.value, "hunter2",
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
                    sensitive: None,
                },
            )
            .await,
        );
    }

    /// Seed a user through the repo and hand back the id it minted.
    async fn seed_user(ctx: &impl Context, email: &str) -> String {
        users::insert(
            ctx,
            users::NewUser {
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
        .id
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
        let u2 = seed_user(&ctx, "u2@example.com").await;

        expect_ok(set_user_disabled(&ctx, &msg, &u2, true).await);
        assert_eq!(audit_count(&ctx, "user.disable").await, 1);
    }

    /// P2c: disable, soft-delete, and a `disabled`-touching field update must
    /// each bump the target user's auth_version so already-issued access JWTs
    /// stop authenticating (`crate::crypto::extract_auth_meta`'s check). A
    /// field update that does NOT touch `disabled` must NOT bump — a plain
    /// name/avatar edit isn't a security-relevant lifecycle change.
    #[tokio::test]
    async fn user_lifecycle_mutations_bump_auth_version() {
        use crate::blocks::auth::repo::users;

        let ctx = admin_ctx().await;
        crate::blocks::auth::migrations::apply(&ctx)
            .await
            .expect("apply auth migrations");
        let msg = admin_msg("update", "/admin/users/admin_1");

        let disable_me = seed_user(&ctx, "d@example.com").await;
        assert_eq!(users::auth_version(&ctx, &disable_me).await.unwrap(), 0);
        expect_ok(set_user_disabled(&ctx, &msg, &disable_me, true).await);
        assert_eq!(
            users::auth_version(&ctx, &disable_me).await.unwrap(),
            1,
            "disable must bump auth_version"
        );

        let delete_me = seed_user(&ctx, "del@example.com").await;
        assert_eq!(users::auth_version(&ctx, &delete_me).await.unwrap(), 0);
        expect_ok(delete_user(&ctx, &msg, &delete_me).await);
        assert_eq!(
            users::auth_version(&ctx, &delete_me).await.unwrap(),
            1,
            "soft-delete must bump auth_version"
        );

        let patch_me = seed_user(&ctx, "p@example.com").await;
        let mut body: HashMap<String, serde_json::Value> = HashMap::new();
        body.insert("name".to_string(), serde_json::json!("New Name"));
        expect_ok(update_user_fields(&ctx, &msg, &patch_me, &body).await);
        assert_eq!(
            users::auth_version(&ctx, &patch_me).await.unwrap(),
            0,
            "a field update that doesn't touch `disabled` must not bump auth_version"
        );

        let mut disable_body: HashMap<String, serde_json::Value> = HashMap::new();
        disable_body.insert("disabled".to_string(), serde_json::json!(true));
        expect_ok(update_user_fields(&ctx, &msg, &patch_me, &disable_body).await);
        assert_eq!(
            users::auth_version(&ctx, &patch_me).await.unwrap(),
            1,
            "a field update that DOES touch `disabled` must bump auth_version"
        );
    }

    /// A body naming a column outside [`AdminUserPatch`]'s three fields
    /// changes nothing: the whitelist is the type, so `role` and `email` are
    /// not reachable from a `PATCH` at all.
    #[tokio::test]
    async fn admin_patch_ignores_columns_outside_the_whitelist() {
        let ctx = admin_ctx().await;
        crate::blocks::auth::migrations::apply(&ctx)
            .await
            .expect("apply auth migrations");
        let msg = admin_msg("update", "/admin/users/admin_1");
        let id = seed_user(&ctx, "whitelist@example.com").await;

        let mut body: HashMap<String, serde_json::Value> = HashMap::new();
        body.insert("role".to_string(), serde_json::json!("admin"));
        body.insert("email".to_string(), serde_json::json!("attacker@evil.test"));
        body.insert("deleted_at".to_string(), serde_json::json!("2026-01-01"));
        body.insert("name".to_string(), serde_json::json!("Renamed"));
        let row = expect_ok(update_user_fields(&ctx, &msg, &id, &body).await);

        assert_eq!(row.name.as_deref(), Some("Renamed"), "name is writable");
        assert_eq!(row.role, "user", "role is not an admin-writable field");
        assert_eq!(
            row.email, "whitelist@example.com",
            "email is not an admin-writable field"
        );
        assert!(
            !row.is_deleted(),
            "deleted_at is not an admin-writable field"
        );
    }

    /// `{"disabled": 1}` — the shape the admin UI has sent — still disables.
    #[tokio::test]
    async fn admin_patch_accepts_an_integer_disabled_flag() {
        let ctx = admin_ctx().await;
        crate::blocks::auth::migrations::apply(&ctx)
            .await
            .expect("apply auth migrations");
        let msg = admin_msg("update", "/admin/users/admin_1");
        let id = seed_user(&ctx, "intflag@example.com").await;

        let mut body: HashMap<String, serde_json::Value> = HashMap::new();
        body.insert("disabled".to_string(), serde_json::json!(1));
        let row = expect_ok(update_user_fields(&ctx, &msg, &id, &body).await);
        assert!(row.disabled);
        assert_eq!(
            users::auth_version(&ctx, &id).await.unwrap(),
            1,
            "an integer disabled flag must still count as touching the flag"
        );
    }
}

/// The admin variables surface must refuse keys the runtime owns.
#[cfg(test)]
mod runtime_key_guard_tests {
    use super::*;
    use crate::{
        config_vars::APP_NAME_KEY,
        test_support::{admin_msg, TestContext},
    };

    async fn admin_ctx() -> TestContext {
        let ctx = TestContext::new()
            .await
            .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
        crate::blocks::admin::migrations::apply(&ctx)
            .await
            .expect("apply admin migrations");
        ctx
    }

    /// An internal adapter key cannot be created through the admin API.
    ///
    /// `blocks::config` never serves `__…__` keys from the variables table —
    /// the browser's `__IMPRESSPRESS_RUNTIME_KIND__ = "browser"` marker is what
    /// keeps Stripe secret-key operations off in a visitor's browser. Storing
    /// one therefore produces a settings row that no reader will ever honour:
    /// the admin sees a value that does nothing, which is the silent no-op this
    /// work exists to remove. Refuse it at the write instead.
    #[tokio::test]
    async fn create_refuses_an_internal_runtime_key() {
        let ctx = admin_ctx().await;
        let msg = admin_msg("create", "/admin/settings");

        let result = create_variable(
            &ctx,
            &msg,
            "__IMPRESSPRESS_RUNTIME_KIND__",
            "server",
            None,
            None,
            false,
        )
        .await;

        assert!(
            result.is_err(),
            "an internal runtime key must be refused, not stored as a dead row"
        );
        assert!(
            variables::get_by_key(&ctx, "__IMPRESSPRESS_RUNTIME_KIND__")
                .await
                .expect("read back")
                .is_none(),
            "a refused create must leave no row"
        );
    }

    /// Same rule on the update surface, for an infrastructure key.
    ///
    /// `IMPRESSPRESS_*` without `__` is infrastructure and never in the
    /// database by the repo's naming convention.
    #[tokio::test]
    async fn update_refuses_an_infrastructure_key() {
        let ctx = admin_ctx().await;
        let msg = admin_msg("update", "/admin/settings");

        let result = update_variable(
            &ctx,
            &msg,
            crate::migration_helper::RUN_MIGRATIONS_KEY,
            VariableUpdate {
                value: Some("1"),
                description: None,
                sensitive: None,
            },
        )
        .await;

        assert!(
            result.is_err(),
            "an infrastructure key must be refused on the update surface too"
        );
    }

    /// Ordinary admin-editable keys are untouched by the guard.
    #[tokio::test]
    async fn an_ordinary_shared_key_is_still_accepted() {
        let ctx = admin_ctx().await;
        let msg = admin_msg("update", "/admin/settings");

        let row = update_variable(
            &ctx,
            &msg,
            APP_NAME_KEY,
            VariableUpdate {
                value: Some("Acme"),
                description: None,
                sensitive: None,
            },
        )
        .await
        // `OutputStream` has no `Debug`, so `expect` is unavailable here.
        .unwrap_or_else(|_| panic!("a shared key must still be writable"));
        assert_eq!(row.value, "Acme");
    }
}
