//! What a failed database call answers on the user portal's routes.
//!
//! A WRAP `PermissionDenied` — a [`wafer_run::ResourceGrant`] the deployment
//! is missing, or a row guard that refused — is a 403, and a quota is a 429,
//! the same as on every other block (`crud::db_error`). Each test drives a
//! real route through [`UserPortalBlock`]'s own dispatch over a
//! `FailingDbOpContext` that refuses the table the site under test reads or
//! writes, so every earlier step of the route runs for real.
//!
//! An htmx fragment route must end in the door's own "Access denied": the
//! status alone would also match a route gate's refusal, which carries its
//! own message. A full page must be the styled 403 `ui::refused_response`
//! draws ("Go home"), not the sign-in 403 `ui::forbidden_response` draws.

use wafer_run::{
    streams::output::TerminalNotResponse, Block, ErrorCode, InputStream, Message, OutputStream,
    WaferError,
};

use super::{
    test_support::{browser_request, routed},
    UserPortalBlock, TABLE,
};
use crate::{
    blocks::auth::repo::{local_credentials, provider_links, sessions, tokens, users},
    test_support::{
        admin_msg, anon_msg, auth_msg, output_http_json, output_http_status, output_json,
        FailingDbOpContext, TestContext,
    },
};

const USER: &str = "user-a";

/// Every database op on `table`.
fn every_op_on(table: &'static str) -> Vec<(&'static str, &'static str)> {
    wafer_block::ServiceOp::DATABASE_OPS
        .iter()
        .map(|op| (*op, table))
        .collect()
}

/// `ctx` with every op on `table` refused the way WRAP refuses a call its
/// caller holds no grant for.
fn denied(ctx: &TestContext, table: &'static str) -> FailingDbOpContext {
    FailingDbOpContext::failing_with(
        ctx.clone(),
        every_op_on(table),
        WaferError::new(
            ErrorCode::PermissionDenied,
            "WRAP: impresspress/userportal holds no grant on this table",
        ),
    )
}

/// An htmx request through the block's own `handle`.
async fn fragment(ctx: &dyn wafer_run::context::Context, msg: Message, body: &str) -> OutputStream {
    UserPortalBlock::new()
        .handle(
            ctx,
            routed(msg),
            InputStream::from_bytes(body.as_bytes().to_vec()),
        )
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
    msg: Message,
) {
    let path = msg.path().to_string();
    let (status, html) = browser_request(ctx, msg, "").await;
    if status != 403 || !html.contains("Go home") {
        misses.push(format!("{path}: {status} {html}"));
    }
}

async fn fixture() -> TestContext {
    let ctx = TestContext::with_userportal().await;
    ctx.seed_auth_user(USER).await;
    ctx
}

/// A provider link for `user`.
async fn link(ctx: &TestContext, user: &str, provider: &str) {
    provider_links::upsert(
        ctx,
        provider_links::NewLink {
            provider,
            provider_ref: &format!("{user}-{provider}"),
            user_id: user,
            provider_login: user,
        },
    )
    .await
    .expect("seed a provider link");
}

// --- full pages ------------------------------------------------------------

/// A page whose read was refused is the styled 403, not the 500 page.
#[tokio::test]
async fn page_read_denials_are_the_403_page() {
    let mut misses = Vec::new();
    let ctx = fixture().await;
    for (table, msg) in [
        (TABLE, auth_msg("retrieve", "/b/userportal/", USER)),
        (
            users::TABLE,
            auth_msg("retrieve", "/b/userportal/profile", USER),
        ),
        (
            sessions::TABLE,
            auth_msg("retrieve", "/b/userportal/sessions", USER),
        ),
        (
            provider_links::TABLE,
            auth_msg("retrieve", "/b/userportal/security", USER),
        ),
        // The verification flag, read after the links.
        (
            users::TABLE,
            auth_msg("retrieve", "/b/userportal/security", USER),
        ),
        (TABLE, admin_msg("retrieve", "/b/userportal/admin/buttons")),
    ] {
        assert_refused_page(&mut misses, &denied(&ctx, table), msg).await;
    }
    report(misses);
}

/// `inner`, with every call to the config service refused with `error`. The
/// branding keys are shared (`WAFER_RUN_SHARED__*`), so no WRAP grant can
/// withhold them from this block; what the config service can still answer is
/// a refusal from the store under it, and this injects one at that boundary.
#[derive(Clone)]
struct RefusingConfig {
    inner: TestContext,
    error: WaferError,
}

#[async_trait::async_trait]
impl wafer_run::context::Context for RefusingConfig {
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
        if name == "wafer-run/config" {
            return OutputStream::error(self.error.clone());
        }
        self.inner.call_block(name, msg, input).await
    }

    fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    fn registered_blocks(&self) -> &[wafer_run::BlockInfo] {
        self.inner.registered_blocks()
    }

    fn config_get(&self, key: &str) -> Option<&str> {
        self.inner.config_get(key)
    }

    fn clone_arc(&self) -> std::sync::Arc<dyn wafer_run::context::Context> {
        std::sync::Arc::new(self.clone())
    }
}

/// The branding form reads every value through the config service; a refusal
/// there is the refusal page, not the 500 page.
#[tokio::test]
async fn branding_settings_refusal_is_the_refusal_page() {
    for (code, status, copy) in [
        (ErrorCode::PermissionDenied, 403, "Go home"),
        (ErrorCode::ResourceExhausted, 429, "over its usage limit"),
        (ErrorCode::AlreadyExists, 409, "Already exists"),
    ] {
        let ctx = RefusingConfig {
            inner: TestContext::with_userportal().await,
            error: WaferError::new(code, "refused by the config store"),
        };
        let (got, html) = browser_request(
            &ctx,
            admin_msg("retrieve", "/b/userportal/admin/settings"),
            "",
        )
        .await;
        assert_eq!(got, status, "{code:?}: {html}");
        assert!(html.contains(copy), "{code:?}: {html}");
        assert!(
            html.contains("Go home"),
            "the styled page, {code:?}: {html}"
        );
        assert!(!html.contains("<form"), "{code:?}: {html}");
    }
}

// --- pages/sessions.rs -------------------------------------------------------

#[tokio::test]
async fn session_revoke_denials_are_403() {
    let mut misses = Vec::new();
    let ctx = fixture().await;
    sessions::insert(
        &ctx,
        sessions::NewSession {
            family: "fam-1".into(),
            user_id: USER.into(),
            auth_method: "password".into(),
            expires_at: "2099-01-01T00:00:00Z".into(),
        },
    )
    .await
    .expect("seed a session");
    tokens::insert(&ctx, USER, "fam-1", "fam-1", 0, "2099-01-01T00:00:00Z")
        .await
        .expect("seed a refresh row");
    let path = "/b/userportal/sessions/fam-1";
    // The ownership lookup, the family revoke, then the session row delete.
    for (failing, step) in [
        (denied(&ctx, sessions::TABLE), "lookup"),
        (denied(&ctx, tokens::TABLE), "revoke"),
        (denied(&ctx, sessions::TABLE).after_passing(1), "delete"),
    ] {
        assert_wrap_denial(
            &mut misses,
            fragment(&failing, auth_msg("delete", path, USER), "").await,
            &format!("{path} ({step})"),
        )
        .await;
    }
    report(misses);
}

// --- pages/security.rs -------------------------------------------------------

#[tokio::test]
async fn provider_unlink_denials_are_403() {
    let mut misses = Vec::new();
    let ctx = fixture().await;
    // Two links, so the unlink needs no password check.
    link(&ctx, USER, "github").await;
    link(&ctx, USER, "google").await;
    // One link and no password: the password check decides.
    ctx.seed_auth_user("user-b").await;
    link(&ctx, "user-b", "github").await;

    let path = "/b/userportal/security/providers/github";
    for (failing, user, step) in [
        (denied(&ctx, provider_links::TABLE), USER, "list"),
        (
            denied(&ctx, local_credentials::TABLE),
            "user-b",
            "password check",
        ),
        (
            denied(&ctx, provider_links::TABLE).after_passing(1),
            USER,
            "delete",
        ),
    ] {
        assert_wrap_denial(
            &mut misses,
            fragment(&failing, auth_msg("delete", path, user), "").await,
            &format!("{path} ({step})"),
        )
        .await;
    }
    report(misses);
}

// --- pages/admin_buttons.rs --------------------------------------------------

#[tokio::test]
async fn button_write_denials_are_403() {
    let mut misses = Vec::new();
    let ctx = fixture().await;
    let form = "label=Files&path=%2Fb%2Fstorage%2F&icon=folder";
    let failing = denied(&ctx, TABLE);
    assert_wrap_denial(
        &mut misses,
        fragment(
            &failing,
            admin_msg("create", "/b/userportal/admin/buttons"),
            form,
        )
        .await,
        "POST /b/userportal/admin/buttons",
    )
    .await;

    let record = wafer_core::clients::database::create(
        &ctx,
        TABLE,
        crate::util::json_map(serde_json::json!({
            "label": "Files", "path": "/b/storage/", "icon": "folder", "sort_order": 0,
        })),
    )
    .await
    .expect("seed a button");
    let path = format!("/b/userportal/admin/buttons/{}", record.id);
    assert_wrap_denial(
        &mut misses,
        fragment(&failing, admin_msg("update", &path), form).await,
        &format!("PATCH {path}"),
    )
    .await;
    report(misses);
}

/// The id is the caller's, so an update of a button that is not there is
/// their 404, not a fault.
#[tokio::test]
async fn updating_a_missing_button_is_404() {
    let ctx = fixture().await;
    let out = fragment(
        &ctx,
        admin_msg("update", "/b/userportal/admin/buttons/no-such-button"),
        "label=Files&path=%2Fb%2Fstorage%2F",
    )
    .await;
    assert_eq!(output_http_status(out).await, 404);
}

/// A button write that landed and then could not re-read the table answers
/// the swappable notice, saying access was denied — never the denial's text.
#[tokio::test]
async fn button_list_reread_denial_is_the_classified_notice() {
    let ctx = fixture().await;
    let failing = FailingDbOpContext::failing_with(
        ctx.clone(),
        vec![("database.list", TABLE)],
        WaferError::new(
            ErrorCode::PermissionDenied,
            "WRAP: impresspress/userportal holds no grant on this table",
        ),
    );
    let mut msg = admin_msg("create", "/b/userportal/admin/buttons");
    msg.set_meta("http.header.hx-request", "true");
    let parts = wafer_block::http_codec::collect_http_response(
        fragment(
            &failing,
            msg,
            "label=Files&path=%2Fb%2Fstorage%2F&icon=folder",
        )
        .await,
    )
    .await;
    let html = String::from_utf8_lossy(&parts.body);
    assert_eq!(parts.status, 200, "{html}");
    assert!(
        html.contains("could not be loaded: access to it was denied"),
        "{html}"
    );
    assert!(!html.contains("holds no grant"), "{html}");
}

// --- the public config -----------------------------------------------------

/// `GET /b/userportal/config` is public, so its body is exactly the portal's
/// settings: every value a plain string or bool, never a serialised `Result`
/// (`{"Ok": …}`) around one.
#[tokio::test]
async fn the_public_config_is_the_settings_themselves() {
    let mut ctx = fixture().await;
    ctx.set_config(crate::config_vars::APP_NAME_KEY, "Acme");
    ctx.set_config(crate::config_vars::ALLOW_SIGNUP_KEY, "false");
    let out = fragment(&ctx, anon_msg("retrieve", "/b/userportal/config"), "").await;
    assert_eq!(
        output_json(out).await,
        serde_json::json!({
            "logo_url": "",
            "app_name": "Acme",
            "primary_color": "",
            "enable_oauth": "false",
            "allow_signup": "false",
            "show_powered_by": true,
            "features": {
                "files": true,
                "products": true,
                "user_products": "false",
                "legal_pages": true,
                "userportal": true,
            }
        }),
    );
}

/// A refused config read answers the classified 403 on this public route —
/// neither a default in the refused value's place nor the refusal's own
/// text, which names grants and blocks.
#[tokio::test]
async fn a_refused_config_read_is_the_classified_denial() {
    let mut ctx = fixture().await;
    ctx.refuse_config_reads(WaferError::new(
        ErrorCode::PermissionDenied,
        "WRAP: impresspress/userportal holds no grant on wafer-run/config",
    ));
    let out = fragment(&ctx, anon_msg("retrieve", "/b/userportal/config"), "").await;
    assert_eq!(
        output_http_json(out).await,
        serde_json::json!({ "error": "PermissionDenied", "message": "Access denied" }),
    );
}
