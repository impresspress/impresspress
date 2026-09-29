//! What a failed database call answers on the admin routes.
//!
//! A WRAP `PermissionDenied` — a [`wafer_run::ResourceGrant`] the deployment
//! is missing, or a row guard that refused — is a 403, and a quota is a 429,
//! the same as on every other block (`crud::db_error`). Each test drives a
//! real route through [`AdminBlock`]'s own dispatch over a
//! `FailingDbOpContext` that refuses the table the site under test reads or
//! writes, so every earlier step of the route runs for real.
//!
//! A JSON route must end in the door's own "Access denied": the status alone
//! would also match a route gate's refusal, which carries its own message. A
//! full page must be the styled 403 `ui::refused_response` draws ("Go home"),
//! not the sign-in 403 `ui::forbidden_response` draws for a missing role.

use wafer_run::{
    streams::output::TerminalNotResponse, Block, ErrorCode, InputStream, OutputStream, WaferError,
};

use super::{
    iam::{PERMISSIONS_TABLE, ROLES_TABLE},
    logs::{AUDIT_LOGS_TABLE, STORAGE_ACCESS_LOGS_TABLE},
    test_support::{browser_request, routed},
    AdminBlock,
};
use crate::{
    blocks::auth::repo::{api_keys, users},
    platform_state::{block_settings, request_logs, user_roles, variables, wrap_grants},
    test_support::{admin_msg, output_json, FailingDbOpContext, TestContext},
};

/// The user the admin acts on; never the admin itself, so no self-lockout
/// guard answers first.
const TARGET: &str = "u-target";

/// Every database op on `table`: the site under test fails whichever call it
/// makes, and an earlier call on another table still runs.
fn every_op_on(table: &'static str) -> Vec<(&'static str, &'static str)> {
    wafer_block::ServiceOp::DATABASE_OPS
        .iter()
        .map(|op| (*op, table))
        .collect()
}

/// `ctx` with `ops` refused the way WRAP refuses a call its caller holds no
/// grant for.
fn denied(ctx: &TestContext, ops: Vec<(&'static str, &'static str)>) -> FailingDbOpContext {
    FailingDbOpContext::failing_with(
        ctx.clone(),
        ops,
        WaferError::new(
            ErrorCode::PermissionDenied,
            "WRAP: impresspress/admin holds no grant on this table",
        ),
    )
}

/// A JSON request through the admin block's own `handle`.
async fn api(
    ctx: &dyn wafer_run::context::Context,
    action: &str,
    path: &str,
    body: &str,
) -> OutputStream {
    let mut msg = routed(admin_msg(action, path));
    msg.set_meta("http.header.accept", "application/json");
    AdminBlock::new()
        .handle(ctx, msg, InputStream::from_bytes(body.as_bytes().to_vec()))
        .await
}

/// Records a miss unless the request ended in the 403
/// `crud::db_error_internal` gives a WRAP denial: `PermissionDenied` with the
/// door's own "Access denied". A test checks every site before it fails, so
/// one run names every site that answers something else.
async fn assert_wrap_denial(misses: &mut Vec<String>, out: OutputStream, route: &str) {
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(error))
            if (error.code, error.message.as_str())
                == (ErrorCode::PermissionDenied, "Access denied") => {}
        Err(TerminalNotResponse::Error(error)) => {
            misses.push(format!("{route}: {:?} {:?}", error.code, error.message))
        }
        Ok(_) => misses.push(format!("{route}: a response, not a WRAP denial")),
        Err(_) => misses.push(format!("{route}: another terminal, not a WRAP denial")),
    }
}

/// Fails with every recorded miss.
fn report(misses: Vec<String>) {
    assert!(
        misses.is_empty(),
        "expected the database door's WRAP denial at every site:\n{}",
        misses.join("\n")
    );
}

/// Records a miss unless the page answered the styled 403 a refused read
/// gets.
async fn assert_refused_page(
    misses: &mut Vec<String>,
    ctx: &dyn wafer_run::context::Context,
    path: &str,
) {
    let parts = browser_request(ctx, page_msg(path)).await;
    let html = String::from_utf8_lossy(&parts.body);
    if parts.status != 403 || !html.contains("Go home") || shows_the_denial(&html) {
        misses.push(format!("{path}: {} {html}", parts.status));
    }
}

/// A `GET` of `path` as the route table binds it, its `?k=v` query moved into
/// the request meta the way the HTTP adapters put it there.
fn page_msg(path: &str) -> wafer_run::Message {
    let (route, query) = path.split_once('?').unwrap_or((path, ""));
    let mut msg = routed(admin_msg("retrieve", route));
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        msg.set_meta(format!("req.query.{key}"), value);
    }
    msg
}

/// Whether `html` carries the WRAP denial's own text — the grant it names —
/// which is logged, never shown.
fn shows_the_denial(html: &str) -> bool {
    html.contains("holds no grant") || html.contains("WRAP")
}

/// An admin fixture with a user to act on, a role (`editor`) that user
/// holds, and the grant's id.
async fn fixture() -> (TestContext, String) {
    let ctx = TestContext::with_auth()
        .await
        .running_as(crate::blocks::admin::ADMIN_BLOCK_ID);
    ctx.seed_auth_user(TARGET).await;
    let created = output_json(
        api(
            &ctx,
            "create",
            "/b/admin/api/iam/roles",
            r#"{"name":"editor"}"#,
        )
        .await,
    )
    .await;
    assert_eq!(created["name"], "editor", "{created}");
    let grant = match user_roles::assign(&ctx, TARGET, "editor", "admin_1")
        .await
        .expect("assign editor")
    {
        user_roles::Assigned::Created(row) => row.id,
        user_roles::Assigned::AlreadyAssigned => panic!("fresh fixture"),
    };
    (ctx, grant)
}

// --- iam.rs ----------------------------------------------------------------

#[tokio::test]
async fn iam_reads_keep_a_wrap_denial() {
    let mut misses = Vec::new();
    let (ctx, _) = fixture().await;
    for (table, path) in [
        (ROLES_TABLE, "/b/admin/api/iam/roles"),
        (PERMISSIONS_TABLE, "/b/admin/api/iam/permissions"),
        (user_roles::TABLE, "/b/admin/api/iam/user-roles"),
    ] {
        let failing = denied(&ctx, every_op_on(table));
        assert_wrap_denial(&mut misses, api(&failing, "retrieve", path, "").await, path).await;
    }

    // One user's grants rather than the capped list of everyone's.
    let failing = denied(&ctx, every_op_on(user_roles::TABLE));
    let mut msg = routed(admin_msg("retrieve", "/b/admin/api/iam/user-roles"));
    msg.set_meta("http.header.accept", "application/json");
    msg.set_meta("req.query.user_id", TARGET);
    assert_wrap_denial(
        &mut misses,
        AdminBlock::new()
            .handle(&failing, msg, InputStream::empty())
            .await,
        "/b/admin/api/iam/user-roles?user_id=u-target",
    )
    .await;
    report(misses);
}

/// The plan's named case. `ops::create_role` already classified its insert
/// through `taken_key_or_db_error`, so this one passed before the change too:
/// it guards the route, it does not prove the fix.
#[tokio::test]
async fn role_create_denial_is_403() {
    let mut misses = Vec::new();
    let (ctx, _) = fixture().await;
    let failing = denied(&ctx, every_op_on(ROLES_TABLE));
    assert_wrap_denial(
        &mut misses,
        api(
            &failing,
            "create",
            "/b/admin/api/iam/roles",
            r#"{"name":"auditor"}"#,
        )
        .await,
        "POST /b/admin/api/iam/roles",
    )
    .await;
    report(misses);
}

/// A duplicate role name is the create's own `AlreadyExists`, and that alone
/// is the 409. A create that fails for another reason is a fault even when
/// the name it wrote is taken: nothing re-reads the key to reinterpret the
/// failure, because every backend classifies a duplicate itself (the
/// `DatabaseService` contract). A re-read after the refused write would find
/// `auditor` and answer this `Internal` as a 409.
#[tokio::test]
async fn a_create_that_fails_as_a_fault_is_a_500_even_when_the_name_is_taken() {
    let (ctx, _) = fixture().await;
    let body = r#"{"name":"auditor"}"#;
    output_json(api(&ctx, "create", "/b/admin/api/iam/roles", body).await).await;

    let duplicate = api(&ctx, "create", "/b/admin/api/iam/roles", body).await;
    assert_eq!(
        crate::test_support::output_http_status(duplicate).await,
        409,
        "the duplicate's own AlreadyExists is the conflict"
    );

    let faulty = FailingDbOpContext::failing_with(
        ctx.clone(),
        vec![(wafer_block::ServiceOp::DATABASE_CREATE, ROLES_TABLE)],
        WaferError::new(ErrorCode::Internal, "disk I/O error"),
    );
    let out = api(&faulty, "create", "/b/admin/api/iam/roles", body).await;
    assert_eq!(
        crate::test_support::output_http_status(out).await,
        500,
        "a fault is not reinterpreted as a conflict by reading the key back"
    );
}

/// The grant write, and the session invalidation after it lands (a write to
/// the users table): a refusal at either is the deployment's grant, not an
/// outage.
#[tokio::test]
async fn role_assign_denials_are_403() {
    let mut misses = Vec::new();
    let (ctx, _) = fixture().await;
    let created = api(
        &ctx,
        "create",
        "/b/admin/api/iam/roles",
        r#"{"name":"auditor"}"#,
    )
    .await;
    output_json(created).await;
    for (table, step) in [(user_roles::TABLE, "grant"), (users::TABLE, "invalidation")] {
        let failing = denied(&ctx, every_op_on(table));
        assert_wrap_denial(
            &mut misses,
            api(
                &failing,
                "create",
                "/b/admin/api/iam/user-roles",
                r#"{"user_id":"u-target","role":"auditor"}"#,
            )
            .await,
            &format!("POST /b/admin/api/iam/user-roles ({step})"),
        )
        .await;
    }
    report(misses);
}

#[tokio::test]
async fn role_remove_denials_are_403() {
    let mut misses = Vec::new();
    let (ctx, grant) = fixture().await;
    let path = format!("/b/admin/api/iam/user-roles/{grant}");
    // The grant lookup, the invalidation before the removal, and the one
    // after it.
    for (failing, step) in [
        (denied(&ctx, every_op_on(user_roles::TABLE)), "lookup"),
        (denied(&ctx, every_op_on(users::TABLE)), "bump before"),
        (
            denied(&ctx, every_op_on(users::TABLE)).after_passing(1),
            "bump after",
        ),
    ] {
        assert_wrap_denial(
            &mut misses,
            api(&failing, "delete", &path, "").await,
            &format!("DELETE {path} ({step})"),
        )
        .await;
    }
    report(misses);
}

// --- ops.rs ----------------------------------------------------------------

/// Disable, delete and the field patch each invalidate the user's sessions
/// after the row changed; a denied invalidation is a 403.
#[tokio::test]
async fn user_lifecycle_invalidation_denials_are_403() {
    let mut misses = Vec::new();
    let (ctx, _) = fixture().await;
    let bump = vec![("database.increment_field_where", users::TABLE)];
    for (action, path, body) in [
        ("create", "/b/admin/users/u-target/disable", ""),
        (
            "update",
            "/b/admin/api/users/u-target",
            r#"{"disabled":true}"#,
        ),
        ("delete", "/b/admin/api/users/u-target", ""),
    ] {
        let failing = denied(&ctx, bump.clone());
        assert_wrap_denial(&mut misses, api(&failing, action, path, body).await, path).await;
    }
    report(misses);
}

/// Deleting a held role invalidates every holder before and after revoking
/// the grants; either invalidation refused is a 403.
#[tokio::test]
async fn role_delete_invalidation_denials_are_403() {
    let mut misses = Vec::new();
    let (ctx, _) = fixture().await;
    let roles = output_json(api(&ctx, "retrieve", "/b/admin/api/iam/roles", "").await).await;
    let editor = roles["roles"]
        .as_array()
        .or_else(|| roles["records"].as_array())
        .expect("role list")
        .iter()
        .find(|r| r["name"] == "editor")
        .expect("editor role")["id"]
        .as_str()
        .expect("role id")
        .to_string();
    let path = format!("/b/admin/api/iam/roles/{editor}");
    let bump = vec![("database.increment_field_where", users::TABLE)];
    for (failing, step) in [
        (denied(&ctx, bump.clone()), "bump before"),
        (denied(&ctx, bump.clone()).after_passing(1), "bump after"),
    ] {
        assert_wrap_denial(
            &mut misses,
            api(&failing, "delete", &path, "").await,
            &format!("DELETE {path} ({step})"),
        )
        .await;
    }
    report(misses);
}

#[tokio::test]
async fn variable_release_denials_are_403() {
    let mut misses = Vec::new();
    let (ctx, _) = fixture().await;
    output_json(
        api(
            &ctx,
            "create",
            "/b/admin/api/settings",
            r#"{"key":"MY_SETTING","value":"x"}"#,
        )
        .await,
    )
    .await;
    let path = "/b/admin/api/settings/MY_SETTING/reset-to-environment";
    // The row read, then the release write after it.
    for (failing, step) in [
        (denied(&ctx, every_op_on(variables::TABLE)), "read"),
        (
            denied(&ctx, every_op_on(variables::TABLE)).after_passing(1),
            "write",
        ),
    ] {
        assert_wrap_denial(
            &mut misses,
            api(&failing, "create", path, "").await,
            &format!("{path} ({step})"),
        )
        .await;
    }

    let failing = denied(&ctx, every_op_on(variables::TABLE));
    let path = "/b/admin/variables/reset-pinned-at-upgrade";
    assert_wrap_denial(&mut misses, api(&failing, "create", path, "").await, path).await;
    report(misses);
}

// --- logs.rs, database.rs, settings.rs, mod.rs ------------------------------

#[tokio::test]
async fn json_reads_keep_a_wrap_denial() {
    let mut misses = Vec::new();
    let (ctx, _) = fixture().await;
    for (ops, path) in [
        (every_op_on(AUDIT_LOGS_TABLE), "/b/admin/api/logs"),
        // The table listing is a raw introspection query, which names no
        // collection.
        (
            vec![("database.query_raw", "")],
            "/b/admin/api/database/info",
        ),
        (every_op_on(variables::TABLE), "/b/admin/api/settings"),
        (every_op_on(variables::TABLE), "/b/admin/api/settings/all"),
    ] {
        let failing = denied(&ctx, ops);
        assert_wrap_denial(&mut misses, api(&failing, "retrieve", path, "").await, path).await;
    }
    report(misses);
}

#[tokio::test]
async fn wrap_grant_create_denial_is_403() {
    let mut misses = Vec::new();
    let (ctx, _) = fixture().await;
    let failing = denied(&ctx, every_op_on(wrap_grants::TABLE));
    assert_wrap_denial(
        &mut misses,
        api(
            &failing,
            "create",
            "/b/admin/grants/rules",
            "grantee=impresspress%2Ffiles&resource=impresspress__foo__bar&write=on",
        )
        .await,
        "POST /b/admin/grants/rules",
    )
    .await;
    report(misses);
}

// --- pages/blocks.rs, pages/users.rs, pages/variables.rs --------------------

#[tokio::test]
async fn block_setting_denials_are_403() {
    let mut misses = Vec::new();
    let (ctx, _) = fixture().await;
    block_settings::set_enabled(&ctx, "impresspress/files", true)
        .await
        .expect("seed a block setting");
    let toggle = "/b/admin/blocks/impresspress--files/toggle";
    // The state read, then the write derived from it.
    for (failing, step) in [
        (denied(&ctx, every_op_on(block_settings::TABLE)), "read"),
        (
            denied(&ctx, every_op_on(block_settings::TABLE)).after_passing(1),
            "persist",
        ),
    ] {
        assert_wrap_denial(
            &mut misses,
            api(&failing, "create", toggle, "").await,
            &format!("{toggle} ({step})"),
        )
        .await;
    }

    let detail = "/b/admin/blocks/impresspress--files/detail";
    let failing = denied(&ctx, every_op_on(block_settings::TABLE));
    assert_wrap_denial(
        &mut misses,
        api(&failing, "retrieve", detail, "").await,
        detail,
    )
    .await;
    report(misses);
}

#[tokio::test]
async fn api_key_revoke_denials_are_403() {
    let mut misses = Vec::new();
    let (ctx, _) = fixture().await;
    let key = api_keys::insert(
        &ctx,
        api_keys::NewApiKey {
            user_id: TARGET,
            name: "ci",
            key_hash: "hash-of-the-key",
            key_prefix: "ip_test",
            expires_at: None,
        },
    )
    .await
    .expect("seed an api key");
    let path = format!("/b/admin/api-keys/{}/revoke", key.id);
    for (failing, step) in [
        (denied(&ctx, every_op_on(api_keys::TABLE)), "lookup"),
        (
            denied(&ctx, every_op_on(api_keys::TABLE)).after_passing(1),
            "revoke",
        ),
    ] {
        assert_wrap_denial(
            &mut misses,
            api(&failing, "create", &path, "").await,
            &format!("{path} ({step})"),
        )
        .await;
    }
    report(misses);
}

#[tokio::test]
async fn variable_edit_form_denial_is_403() {
    let mut misses = Vec::new();
    let (ctx, _) = fixture().await;
    let failing = denied(&ctx, every_op_on(variables::TABLE));
    let path = "/b/admin/variables/MY_SETTING/edit";
    assert_wrap_denial(&mut misses, api(&failing, "retrieve", path, "").await, path).await;
    report(misses);
}

// --- full pages ----------------------------------------------------------

/// A page whose read was refused is the styled 403, not the 500 page — an
/// operator reading "Something went wrong" looks for an outage, not a grant.
#[tokio::test]
async fn page_read_denials_are_the_403_page() {
    let mut misses = Vec::new();
    let (ctx, _) = fixture().await;
    for (ops, path) in [
        (every_op_on(STORAGE_ACCESS_LOGS_TABLE), "/b/admin/storage"),
        (every_op_on(block_settings::TABLE), "/b/admin/blocks"),
        (vec![("database.query_raw", "")], "/b/admin/database"),
        (
            every_op_on(wrap_grants::TABLE),
            "/b/admin/settings/permissions",
        ),
        // The tabs that printed the failure's own text into a 200 page.
        (every_op_on(request_logs::TABLE), "/b/admin/logs"),
        (every_op_on(AUDIT_LOGS_TABLE), "/b/admin/logs?tab=audit"),
        (every_op_on(users::TABLE), "/b/admin/users"),
        (every_op_on(ROLES_TABLE), "/b/admin/users?tab=roles"),
        (every_op_on(api_keys::TABLE), "/b/admin/users?tab=api-keys"),
        (every_op_on(variables::TABLE), "/b/admin/settings/variables"),
        (
            every_op_on(variables::TABLE),
            "/b/admin/settings/variables?tab=all",
        ),
    ] {
        assert_refused_page(&mut misses, &denied(&ctx, ops), path).await;
    }
    report(misses);
}

// --- re-renders after a write --------------------------------------------

/// A write that landed and then could not re-read the fragment its control
/// swaps answers the notice htmx can swap: 200, saying the write happened and
/// that access was denied, never the denial's own text.
#[tokio::test]
async fn reread_denials_after_a_write_are_the_classified_notice() {
    let mut misses = Vec::new();
    let (ctx, _) = fixture().await;
    let key = api_keys::insert(
        &ctx,
        api_keys::NewApiKey {
            user_id: TARGET,
            name: "ci",
            key_hash: "hash-of-the-key",
            key_prefix: "ip_test",
            expires_at: None,
        },
    )
    .await
    .expect("seed an api key");
    output_json(
        api(
            &ctx,
            "create",
            "/b/admin/api/settings",
            r#"{"key":"MY_SETTING","value":"x"}"#,
        )
        .await,
    )
    .await;
    let roles = output_json(api(&ctx, "retrieve", "/b/admin/api/iam/roles", "").await).await;
    let editor = roles["roles"]
        .as_array()
        .or_else(|| roles["records"].as_array())
        .expect("role list")
        .iter()
        .find(|r| r["name"] == "editor")
        .expect("editor role")["id"]
        .as_str()
        .expect("role id")
        .to_string();

    let revoke = format!("/b/admin/api-keys/{}/revoke", key.id);
    let delete_role = format!("/b/admin/iam/roles/{editor}");
    // (action, path, form, refused ops, reads of them the write makes first,
    // what the notice says landed)
    type Case<'a> = (
        &'a str,
        String,
        &'a str,
        Vec<(&'static str, &'static str)>,
        usize,
        &'a str,
    );
    let cases: Vec<Case> = vec![
        (
            "create",
            "/b/admin/iam/roles".to_string(),
            "name=auditor",
            vec![("database.list", ROLES_TABLE)],
            0,
            "Role created",
        ),
        (
            "delete",
            delete_role,
            "",
            vec![("database.list", ROLES_TABLE)],
            0,
            "Role deleted",
        ),
        (
            "create",
            revoke,
            "",
            vec![("database.list", api_keys::TABLE)],
            0,
            "API key revoked",
        ),
        (
            "create",
            "/b/admin/variables".to_string(),
            "key=OTHER_SETTING&value=y&sensitive=0",
            vec![("database.list", variables::TABLE)],
            0,
            "Variable created",
        ),
        (
            "update",
            "/b/admin/variables/MY_SETTING".to_string(),
            "value=z&sensitive=0",
            vec![("database.list", variables::TABLE)],
            // The update reads the row's stored flag and its pin first.
            2,
            "Variable updated",
        ),
        (
            "create",
            "/b/admin/users/u-target/disable".to_string(),
            "",
            vec![("database.get", users::TABLE)],
            0,
            "User disabled",
        ),
    ];
    for (action, path, body, ops, passes, done) in cases {
        let failing = denied(&ctx, ops).after_passing(passes);
        let mut msg = routed(admin_msg(action, &path));
        msg.set_meta("http.header.accept", "text/html");
        msg.set_meta("http.header.hx-request", "true");
        let parts = wafer_block::http_codec::collect_http_response(
            AdminBlock::new()
                .handle(
                    &failing,
                    msg,
                    InputStream::from_bytes(body.as_bytes().to_vec()),
                )
                .await,
        )
        .await;
        let html = String::from_utf8_lossy(&parts.body);
        let notice = "could not be reloaded: access to it was denied";
        if parts.status != 200
            || !html.contains(done)
            || !html.contains(notice)
            || shows_the_denial(&html)
        {
            misses.push(format!("{action} {path}: {} {html}", parts.status));
        }
    }
    report(misses);
}

/// The unfiltered grant list reads past its cap only when the table holds
/// more than `db_read::UNPAGED_LIMIT` grants, and only then counts them; a
/// denied count is a 403 like every other read.
#[tokio::test]
async fn grant_count_denial_past_the_cap_is_403() {
    let mut misses = Vec::new();
    let (ctx, _) = fixture().await;
    // Fixture setup, one statement: one grant each for more users than the
    // listing returns.
    wafer_core::clients::database::exec_raw(
        &ctx,
        &format!(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i <= ?) \
             INSERT INTO {} (id, user_id, role, assigned_by, created_at, updated_at) \
             SELECT 'bulk-' || i, 'bulk-user-' || i, 'editor', '', '2026-01-01', '2026-01-01' \
             FROM n",
            user_roles::TABLE
        ),
        &[serde_json::json!(crate::db_read::UNPAGED_LIMIT)],
    )
    .await
    .expect("seed grants past the cap");
    let failing = denied(&ctx, vec![("database.count", user_roles::TABLE)]);
    let path = "/b/admin/api/iam/user-roles";
    assert_wrap_denial(&mut misses, api(&failing, "retrieve", path, "").await, path).await;

    // The control: the same read granted reports the whole count.
    let list = output_json(api(&ctx, "retrieve", path, "").await).await;
    assert_eq!(
        list["total_count"],
        crate::db_read::UNPAGED_LIMIT + 2,
        "the listing must be past its cap for this test to reach the count"
    );
    report(misses);
}

/// Asserts `out` is a 409 whose message is exactly `message`, with nothing of
/// the driver's text (table, column, index) in the body.
async fn expect_named_conflict(out: OutputStream, message: &str, route: &str) {
    let parts = wafer_block::http_codec::collect_http_response(out).await;
    let body: serde_json::Value = serde_json::from_slice(&parts.body).unwrap_or_default();
    assert_eq!(parts.status, 409, "{route}: {body}");
    assert_eq!(
        body["message"],
        serde_json::json!(message),
        "{route}: {body}"
    );
    let text = body.to_string();
    assert!(
        !text.contains("impresspress__admin") && !text.contains("UNIQUE"),
        "{route}: schema leaked: {text}"
    );
}

/// Every admin write that sets a unique name or key says which one is taken,
/// in the one sentence `crud::TakenKey` builds — the record, the field and the
/// value — so no two routes word the same refusal differently.
#[tokio::test]
async fn every_taken_name_or_key_is_a_409_naming_it() {
    let (ctx, _) = fixture().await;

    let role = r#"{"name":"auditor"}"#;
    output_json(api(&ctx, "create", "/b/admin/api/iam/roles", role).await).await;
    expect_named_conflict(
        api(&ctx, "create", "/b/admin/api/iam/roles", role).await,
        "A role with the name \"auditor\" already exists. Choose a different name.",
        "POST /b/admin/api/iam/roles",
    )
    .await;

    let other = output_json(
        api(
            &ctx,
            "create",
            "/b/admin/api/iam/roles",
            r#"{"name":"reviewer"}"#,
        )
        .await,
    )
    .await;
    let other_id = other["id"].as_str().expect("role id");
    expect_named_conflict(
        api(
            &ctx,
            "update",
            &format!("/b/admin/api/iam/roles/{other_id}"),
            role,
        )
        .await,
        "A role with the name \"auditor\" already exists. Choose a different name.",
        "PATCH /b/admin/api/iam/roles/{id}",
    )
    .await;

    let permission = r#"{"name":"reports.read","resource":"reports","actions":["read"]}"#;
    output_json(api(&ctx, "create", "/b/admin/api/iam/permissions", permission).await).await;
    expect_named_conflict(
        api(&ctx, "create", "/b/admin/api/iam/permissions", permission).await,
        "A permission with the name \"reports.read\" already exists. Choose a different name.",
        "POST /b/admin/api/iam/permissions",
    )
    .await;

    let variable = r#"{"key":"SITE_MOTTO","value":"one"}"#;
    output_json(api(&ctx, "create", "/b/admin/api/settings", variable).await).await;
    expect_named_conflict(
        api(&ctx, "create", "/b/admin/api/settings", variable).await,
        "A variable with the key \"SITE_MOTTO\" already exists. Choose a different key.",
        "POST /b/admin/api/settings",
    )
    .await;
}
