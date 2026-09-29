//! Typed request/response contracts for the admin JSON API.
//!
//! These did not exist before this module. Every admin handler returned the
//! database layer's untyped shapes directly — [`RecordList`] (a `{records:
//! [{id, data: {column → value}}], total_count, page, page_size}` envelope) or
//! a bare `HashMap<String, serde_json::Value>` — so the block declared no
//! schemas at all and its JSON API was invisible in `/openapi.json`.
//!
//! Two consequences of that, both closed here:
//!
//! * **The response was whatever the table happened to hold.**
//!   `GET /b/admin/api/users` echoed every column of `wafer_run__auth__users`,
//!   including `verification_token` — the sha256 of a user's email-verification
//!   token. The views below are *closed* field lists built column by column, so
//!   a column added to a table is never published by accident.
//! * **The JSON types were backend-dependent.** `users.email_verified` /
//!   `users.disabled` are `INTEGER` on SQLite/D1 but `BOOLEAN` on Postgres, and
//!   `roles.permissions` is a JSON-encoded `TEXT` column that the SQLite backend
//!   sniffs back into an array while Postgres/D1 hand back the raw string. A
//!   schema could not have been true for all three. Every field below is
//!   normalized through [`RecordExt`], so it now is.
//!
//! The `{records, total_count, page, page_size}` envelope is preserved exactly:
//! only the per-row shape changes, from `{id, data: {…}}` to the flat view.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use wafer_core::clients::database::{Record, RecordList};
use wafer_run::Message;

use crate::{
    blocks::auth::repo::users::{UserPage, UserRow},
    util::RecordExt,
};

// ---------------------------------------------------------------------------
// GET /b/admin/api/users
// ---------------------------------------------------------------------------

// Built column by column from `wafer_run__auth__users`. Three of that table's
// columns are deliberately NOT published, and the reasons live here in a plain
// comment rather than in the doc comment below: a `///` line is published as
// the schema's `description`, and the public contract has no reason to name
// the columns it withholds.
//
// * `verification_token` — `sha256_hex` of the user's email-verification
//   token. Credential material; it has no admin-UI use, and the untyped
//   handler only ever emitted it because it echoed the whole row.
// * `last_verification_sent` — bookkeeping for the verification-email
//   throttle, meaningful only to the signup flow.
// * `auth_version` — the internal JWT-invalidation counter
//   (`blocks::auth::bump_auth_version`); an implementation detail of token
//   revocation, not an account attribute.
//
// A password hash was never among them: credentials live in
// `wafer_run__auth__local_credentials`, a different table this endpoint does
// not read. The two `record.data.remove("password_hash")` lines admin's
// mutation layer used to run were no-ops against a column that is not on this
// table, and are gone; the view is built from a typed `UserRow` now, which has
// no field to strip.
/// A user account as published by the admin API.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AdminUserView {
    /// Stable user identifier.
    pub id: String,
    /// Login email address.
    pub email: String,
    /// Display name shown in the UI.
    pub display_name: String,
    /// Full name, when the user supplied one.
    pub name: Option<String>,
    /// Avatar image URL, when set.
    pub avatar_url: Option<String>,
    /// Legacy single-role column on the user row (`"user"` by default).
    /// Authorization uses `roles`; this field is retained because the column is
    /// still written by the signup path.
    pub role: String,
    /// Role names assigned to this user in `impresspress__admin__user_roles`.
    pub roles: Vec<String>,
    /// Whether the email address has been verified.
    pub email_verified: bool,
    /// Whether the account is disabled (blocked from signing in).
    pub disabled: bool,
    /// RFC 3339 timestamp of the last successful sign-in, if any.
    pub last_login_at: Option<String>,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
    /// RFC 3339 timestamp of the last modification.
    pub updated_at: String,
    /// RFC 3339 soft-delete timestamp. Always `null` in the list response,
    /// which filters on `deleted_at IS NULL`.
    pub deleted_at: Option<String>,
}

impl AdminUserView {
    /// Project a decoded users row plus its resolved role names. The row
    /// comes from `auth::repo::users`, the only place this table's columns
    /// are read; this type is the closed list of what reaches the wire.
    pub fn from_row(row: &UserRow, roles: Vec<String>) -> Self {
        Self {
            id: row.id.clone(),
            email: row.email.clone(),
            display_name: row.display_name.clone(),
            name: row.name.clone(),
            avatar_url: row.avatar_url.clone(),
            role: row.role.clone(),
            roles,
            email_verified: row.email_verified,
            disabled: row.disabled,
            last_login_at: row.last_login_at.clone(),
            created_at: row.created_at.clone(),
            updated_at: row.updated_at.clone(),
            deleted_at: row.deleted_at.clone(),
        }
    }
}

/// Query parameters accepted by `GET /b/admin/api/users`.
///
/// Built by [`Self::from_message`], which is the handler's only source for
/// these values — the type is the parser, not a parallel description of one.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct AdminUserListQuery {
    /// 1-based page number. Values below 1 clamp to 1.
    #[serde(default = "default_page")]
    pub page: u32,
    /// Rows per page, capped at 100.
    #[serde(default = "default_user_page_size")]
    pub page_size: u32,
    /// Case-insensitive `LIKE '%…%'` filter on the email address.
    pub search: Option<String>,
}

impl AdminUserListQuery {
    /// Resolve the query string on `msg`, applying the same defaults and clamps
    /// the handler applied inline before this type existed.
    pub fn from_message(msg: &Message) -> Self {
        let (page, page_size, _) = msg.pagination_params(DEFAULT_USER_PAGE_SIZE as usize);
        Self {
            page: page as u32,
            page_size: page_size as u32,
            search: non_empty(msg.query("search")),
        }
    }
}

/// Response body of `GET /b/admin/api/users`.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct AdminUserListResponse {
    /// Users on this page, newest first.
    pub records: Vec<AdminUserView>,
    /// Total users matching the filter, across all pages.
    pub total_count: i64,
    /// 1-based index of this page.
    pub page: i64,
    /// Rows per page used to compute `page`.
    pub page_size: i64,
}

impl AdminUserListResponse {
    /// Project a page of user rows plus the bulk-fetched
    /// `user_id → [role]` map.
    pub fn from_page(page: &UserPage, roles_by_user: &HashMap<String, Vec<String>>) -> Self {
        Self {
            records: page
                .rows
                .iter()
                .map(|row| {
                    let roles = roles_by_user.get(&row.id).cloned().unwrap_or_default();
                    AdminUserView::from_row(row, roles)
                })
                .collect(),
            total_count: page.total_count,
            page: page.page,
            page_size: page.page_size,
        }
    }
}

// ---------------------------------------------------------------------------
// GET /b/admin/api/iam/roles
// ---------------------------------------------------------------------------

/// A role definition as published by the admin API.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AdminRoleView {
    /// Stable role identifier.
    pub id: String,
    /// Unique role name (`"admin"`, `"user"`, …). This is the value stored in
    /// `user_roles.role` and checked by the auth layer.
    pub name: String,
    /// Human-readable description shown in the IAM UI.
    pub description: String,
    /// Permission names attached to the role. Advisory metadata for the IAM
    /// UI — WRAP grants, not this list, are what the runtime enforces.
    pub permissions: Vec<String>,
    /// Whether this is a built-in role. System roles cannot be renamed or
    /// deleted.
    pub is_system: bool,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
    /// RFC 3339 timestamp of the last modification.
    pub updated_at: String,
}

impl AdminRoleView {
    /// Project an `impresspress__admin__roles` row.
    pub fn from_record(record: &Record) -> Self {
        Self {
            id: record.id.clone(),
            name: record.str_field("name").to_string(),
            description: record.str_field("description").to_string(),
            permissions: record.string_list_field("permissions"),
            is_system: record.bool_field("is_system"),
            created_at: record.str_field("created_at").to_string(),
            updated_at: record.str_field("updated_at").to_string(),
        }
    }
}

/// Response body of `GET /b/admin/api/iam/roles`.
///
/// The endpoint takes no query parameters: it returns every role, sorted by
/// name, up to the handler's fixed 1000-row ceiling.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct AdminRoleListResponse {
    /// Roles, sorted by name ascending.
    pub records: Vec<AdminRoleView>,
    /// Total roles defined.
    pub total_count: i64,
    /// 1-based index of this page. Always 1 — the handler does not paginate.
    pub page: i64,
    /// Rows per page. Always the handler's fixed 1000-row ceiling.
    pub page_size: i64,
}

impl AdminRoleListResponse {
    /// Project a `RecordList` of role rows.
    pub fn from_record_list(list: &RecordList) -> Self {
        Self {
            records: list
                .records
                .iter()
                .map(AdminRoleView::from_record)
                .collect(),
            total_count: list.total_count,
            page: list.page,
            page_size: list.page_size,
        }
    }
}

/// `POST /b/admin/api/iam/roles` request body.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CreateRoleRequest {
    /// Unique role name. This is the value stored in `user_roles.role` and
    /// checked by the auth layer.
    pub name: String,
    /// Human-readable description shown in the IAM UI. Empty when omitted.
    pub description: Option<String>,
    /// Permission names to attach. Advisory metadata for the IAM UI — WRAP
    /// grants, not this list, are what the runtime enforces. None when
    /// omitted.
    pub permissions: Option<Vec<String>>,
}

/// `PATCH /b/admin/api/iam/roles/{id}` request body. Every field is optional
/// and only the ones present are applied. `name` is refused on a system role:
/// renaming `admin` would break auth.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UpdateRoleRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub permissions: Option<Vec<String>>,
}

/// `PATCH /b/admin/api/iam/roles/{id}` response body: the role as it now is.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct AdminRoleUpdateResponse {
    #[serde(flatten)]
    pub role: AdminRoleView,
    /// Present when the update was saved but a rename's grants did not all
    /// follow it: says which still name the old role, and how to move them.
    // A rename rewrites every `user_roles` row naming the old name, one
    // write each; a failure part-way leaves the role renamed and some grants
    // behind. That is still a 200 — the rename happened — and this is how
    // a caller learns what is left (`iam::handle_update_role`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// `DELETE /b/admin/api/iam/roles/{id}` response body.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AdminRoleDeleteResponse {
    /// Always `true`: a delete that did not happen is an error response.
    pub deleted: bool,
}

// ---------------------------------------------------------------------------
// GET /b/admin/api/settings
// ---------------------------------------------------------------------------

// Masking, in full, so a reviewer does not have to reconstruct it from three
// files. Before a value reaches the map, the handler asks
// `crate::util::is_sensitive_key(key, row.sensitive)`, which is true when
// either the row's `sensitive` column is `1` or the KEY is one this build
// knows to hold a secret — it ends in `_SECRET`/`_KEY`, or a declared
// `ConfigVar` for it is `InputType::Password` or `auto_generate`
// (`config_vars::is_sensitive_for_storage`). A true answer substitutes
// `crate::util::MASKED_VALUE` ("********") for the stored value. The
// `sensitive` column is therefore a cache of that answer, not the only copy of
// it: a row an older build stored unflagged for a password-shaped key carrying
// neither suffix — `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_PASSWORD` — is
// masked here before `platform_state::variables::repair_sensitive_flags` ever
// gets to fix the column. The masked string is a fixed width, so it reveals
// nothing about the real value's length either.
//
// The residual gap is an *ad hoc* variable — one this build declares no
// `ConfigVar` for — created through the admin UI with a secret-ish name and the
// `sensitive` checkbox cleared: it matches neither half of the rule and its
// value is published. Nothing here knows such a key holds a secret, which is
// why the create path defaults it to sensitive
// (`config_vars::is_sensitive_by_default_when_created`) and clearing the box is
// a deliberate admin act. That is a property of the create form, not of this
// endpoint, and is unchanged by typing the response.
/// One configuration variable, as published by `GET /b/admin/api/settings`.
///
/// `sensitive` is what makes the masking legible to a reader that is not a
/// person: a bare `"********"` is indistinguishable from a variable whose
/// value really is that string, and a caller cannot tell it must not treat
/// the value as usable.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AdminSettingView {
    /// Variable name, e.g. `WAFER_RUN_SHARED__AUTH__BOOTSTRAP_ADMIN_EMAIL`.
    pub key: String,
    /// The stored value, or `"********"` when `sensitive` is true.
    ///
    /// Typed as `any` rather than `string` because the stored column is text
    /// that the SQLite and D1 backends decode back into JSON when it looks
    /// like an object or an array: a variable holding `["a","b"]` reads back
    /// as an array, one holding `on` reads back as a string.
    pub value: serde_json::Value,
    /// Whether `value` is masked. True when the row carries the sensitive
    /// flag, or the key is one this build knows to hold a secret: it ends in
    /// `_SECRET` or `_KEY`, or its declaration is a password-typed or
    /// auto-generated variable.
    pub sensitive: bool,
}

/// Response body of `GET /b/admin/api/settings`: every configuration
/// variable, sorted by key.
///
/// **Sensitive values are never present.** A variable is treated as sensitive
/// when it is flagged sensitive in the database, or its key ends in `_SECRET`
/// or `_KEY`, or its declaration is a password-typed or auto-generated
/// variable; its value is then replaced with `"********"` before the response
/// is built. Reading this endpoint cannot recover a secret, nor its length.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdminSettingsResponse {
    pub settings: Vec<AdminSettingView>,
}

// ---------------------------------------------------------------------------
// GET /b/admin/api/logs
// ---------------------------------------------------------------------------

/// One audit-log entry: an admin-initiated mutation, recorded whenever an
/// admin changes a user, a role, or a configuration variable.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AdminAuditLogView {
    /// Stable entry identifier.
    pub id: String,
    /// Id of the admin who performed the action. Empty when the action had no
    /// authenticated actor.
    pub user_id: String,
    /// Action name (`"user.delete"`, `"role.create"`, …).
    pub action: String,
    /// Target the action was applied to.
    pub resource: String,
    /// Client IP the action came from, as seen by the request pipeline. Empty
    /// when the pipeline could not determine one.
    pub ip_address: String,
    /// RFC 3339 timestamp the action was recorded at.
    pub created_at: String,
    /// RFC 3339 write timestamp. Audit rows are never updated, so this always
    /// equals `created_at`.
    pub updated_at: String,
}

impl AdminAuditLogView {
    /// Project an `impresspress__admin__audit_logs` row.
    pub fn from_record(record: &Record) -> Self {
        Self {
            id: record.id.clone(),
            user_id: record.str_field("user_id").to_string(),
            action: record.str_field("action").to_string(),
            resource: record.str_field("resource").to_string(),
            ip_address: record.str_field("ip_address").to_string(),
            created_at: record.str_field("created_at").to_string(),
            updated_at: record.str_field("updated_at").to_string(),
        }
    }
}

/// Query parameters accepted by `GET /b/admin/api/logs`.
///
/// Built by [`Self::from_message`], which is the handler's only source for
/// these values.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct AdminAuditLogListQuery {
    /// 1-based page number. Values below 1 clamp to 1.
    #[serde(default = "default_page")]
    pub page: u32,
    /// Rows per page, capped at 100.
    #[serde(default = "default_log_page_size")]
    pub page_size: u32,
    /// Exact-match filter on the acting admin's user id.
    pub user_id: Option<String>,
    /// Exact-match filter on the action name.
    pub action: Option<String>,
    /// `LIKE '%…%'` filter on the affected resource.
    pub resource: Option<String>,
}

impl AdminAuditLogListQuery {
    /// Resolve the query string on `msg`, applying the same defaults and clamps
    /// the handler applied inline before this type existed.
    pub fn from_message(msg: &Message) -> Self {
        let (page, page_size, _) = msg.pagination_params(DEFAULT_LOG_PAGE_SIZE as usize);
        Self {
            page: page as u32,
            page_size: page_size as u32,
            user_id: non_empty(msg.query("user_id")),
            action: non_empty(msg.query("action")),
            resource: non_empty(msg.query("resource")),
        }
    }
}

/// Response body of `GET /b/admin/api/logs`.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct AdminAuditLogListResponse {
    /// Audit entries on this page, newest first.
    pub records: Vec<AdminAuditLogView>,
    /// Total entries matching the filters, across all pages.
    pub total_count: i64,
    /// 1-based index of this page.
    pub page: i64,
    /// Rows per page used to compute `page`.
    pub page_size: i64,
}

impl AdminAuditLogListResponse {
    /// Project a `RecordList` of audit-log rows.
    pub fn from_record_list(list: &RecordList) -> Self {
        Self {
            records: list
                .records
                .iter()
                .map(AdminAuditLogView::from_record)
                .collect(),
            total_count: list.total_count,
            page: list.page,
            page_size: list.page_size,
        }
    }
}

// ---------------------------------------------------------------------------
// Shared query-param plumbing
// ---------------------------------------------------------------------------

/// One row of `GET /b/admin/api/extensions`: a registered block, projected
/// off wafer-run's `BlockInfo`.
///
/// A closed field list for the same reason every view in this module is one
/// — `BlockInfo` carries config keys, collection schemas, endpoint tables and
/// capability grants that this endpoint has never published and must not
/// start publishing because upstream grew a field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AdminExtensionView {
    /// Block name in the canonical `{org}/{block}` form.
    pub name: String,
    /// Semantic version of the block implementation.
    pub version: String,
    /// Interface identifier, e.g. `"http-handler@v1"`.
    pub interface: String,
    /// One-line summary of what the block does.
    pub summary: String,
    /// Whether the block is enabled.
    ///
    /// Read from the boot block-settings snapshot — the same source
    /// `routing::route_to_block`'s feature gate consults — so `false` means
    /// the router answers "endpoint not found" for every one of this block's
    /// routes. A block with no stored row reports `true`.
    pub enabled: bool,
}

/// Default page size for `GET /b/admin/api/users`.
const DEFAULT_USER_PAGE_SIZE: u32 = 20;

/// Default page size for `GET /b/admin/api/logs`.
const DEFAULT_LOG_PAGE_SIZE: u32 = 50;

/// `?page` default, shared by both paginated endpoints.
fn default_page() -> u32 {
    1
}

fn default_user_page_size() -> u32 {
    DEFAULT_USER_PAGE_SIZE
}

fn default_log_page_size() -> u32 {
    DEFAULT_LOG_PAGE_SIZE
}

/// An absent query parameter and an empty one mean the same thing to every
/// admin filter (`msg.query` returns `""` for both), so collapse them onto
/// `None` rather than letting `Some("")` reach a `LIKE '%%'`.
fn non_empty(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}
