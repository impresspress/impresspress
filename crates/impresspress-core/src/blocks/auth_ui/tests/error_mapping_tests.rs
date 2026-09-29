//! What a failed database call answers on the auth routes.
//!
//! A WRAP `PermissionDenied` — a [`wafer_run::ResourceGrant`] the deployment
//! is missing, or a row guard that refused — is a 403, and a quota is a 429,
//! the same as on every other block (`crud::db_error`). The auth handlers
//! answered both as `500 Internal server error (ref: …)`, which an operator
//! cannot tell from a corrupt row and a caller cannot tell from an outage.
//!
//! Each test drives a real route through [`AuthUiBlock`]'s own dispatch (the
//! route table, then the rate limiter, then the handler) over a
//! `FailingDbOpContext` that refuses exactly the `(action, table)` the site
//! under test calls, so every earlier step of the route runs for real. The
//! `Internal` pairs pin that an outage is still the 500 it always was.
//!
//! The exception is forgot-password and resend-verification (end of file):
//! public endpoints with one constant body, where any refusal a registered
//! address alone can reach must answer that body, not a 403 or 500.

use serde_json::json;
use wafer_run::{context::Context, Block, ErrorCode, InputStream, Message, WaferError};

use crate::{
    blocks::{
        auth::repo::{api_keys, bootstrap_tokens, local_credentials, orgs, tokens, users},
        auth_ui::AuthUiBlock,
    },
    test_support::{
        anon_msg, auth_msg, output_http_status, output_json, FailingDbOpContext, TestContext,
    },
};

const PASSWORD: &str = "correct-horse-battery";

/// The refusal WRAP answers a call its caller holds no grant for.
fn wrap_denial() -> WaferError {
    WaferError::new(
        ErrorCode::PermissionDenied,
        "WRAP: impresspress/auth-ui holds no grant on this table",
    )
}

/// `ctx` with every `(action, table)` in `failing` refused by WRAP.
fn denied(ctx: TestContext, failing: Vec<(&'static str, &'static str)>) -> FailingDbOpContext {
    FailingDbOpContext::failing_with(ctx, failing, wrap_denial())
}

/// `ctx` with every `database.batch` that writes to `table` refused by WRAP.
///
/// A batch carries one op per row, each naming its own collection, so the
/// table-matching `FailingDbOpContext` (which reads a single top-level
/// `collection`) cannot select one. Account creation is a batch — the account
/// row with its credentials or provider link — and this is how its denial is
/// reached.
#[derive(Clone)]
struct BatchDenied {
    inner: TestContext,
    table: &'static str,
}

#[async_trait::async_trait]
impl Context for BatchDenied {
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

    async fn call_block(
        &self,
        name: &str,
        msg: Message,
        input: InputStream,
    ) -> wafer_run::OutputStream {
        if name != "wafer-run/database" || msg.action() != "database.batch" {
            return self.inner.call_block(name, msg, input).await;
        }
        let bytes = match input.collect_to_bytes().await {
            Ok(bytes) => bytes,
            Err(e) => return wafer_run::OutputStream::error(e),
        };
        let request: wafer_block::wire::database::BatchRequest =
            wafer_block::codec::decode(&bytes).expect("a batch request");
        if request.ops.iter().any(|op| op.collection() == self.table) {
            return wafer_run::OutputStream::error(wrap_denial());
        }
        self.inner
            .call_block(name, msg, InputStream::from_bytes(bytes))
            .await
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

    fn clone_arc(&self) -> std::sync::Arc<dyn Context> {
        std::sync::Arc::new(self.clone())
    }
}

/// `msg` with a client address, as the pipeline stamps one: the IP-keyed
/// rate-limit buckets and the outbound-mail limiter key on it.
fn from_client(mut msg: Message) -> Message {
    msg.set_meta(wafer_block::meta::META_REQ_CLIENT_IP, "203.0.113.40");
    msg
}

/// Dispatch `msg` through the auth-ui block and answer the HTTP status.
async fn status(ctx: &dyn Context, msg: Message, body: serde_json::Value) -> u16 {
    let bytes = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(&body).expect("serialize request body")
    };
    output_http_status(
        AuthUiBlock::default()
            .handle(ctx, msg, InputStream::from_bytes(bytes))
            .await,
    )
    .await
}

/// Sign `email` up through the real signup route. Verification is not
/// required by default, so the account is signed in on the spot and the
/// response carries its tokens.
async fn signup(ctx: &TestContext, email: &str) -> serde_json::Value {
    let body = serde_json::to_vec(&json!({"email": email, "password": PASSWORD}))
        .expect("serialize signup body");
    output_json(
        AuthUiBlock::default()
            .handle(
                ctx,
                from_client(anon_msg("create", "/b/auth/api/signup")),
                InputStream::from_bytes(body),
            )
            .await,
    )
    .await
}

fn login_body(email: &str) -> serde_json::Value {
    json!({"email": email, "password": PASSWORD})
}

fn login_msg() -> Message {
    from_client(anon_msg("create", "/b/auth/api/login"))
}

// --- api/login.rs ----------------------------------------------------------

#[tokio::test]
async fn login_user_lookup_denial_is_403_not_500() {
    let ctx = denied(
        TestContext::with_auth_and_crypto().await,
        vec![
            ("database.get", users::TABLE),
            ("database.list", users::TABLE),
        ],
    );
    assert_eq!(
        status(&ctx, login_msg(), login_body("nobody@example.com")).await,
        403
    );
}

#[tokio::test]
async fn login_user_lookup_outage_is_still_500() {
    let ctx = FailingDbOpContext::new(
        TestContext::with_auth_and_crypto().await,
        vec![
            ("database.get", users::TABLE),
            ("database.list", users::TABLE),
        ],
    );
    assert_eq!(
        status(&ctx, login_msg(), login_body("nobody@example.com")).await,
        500
    );
}

/// `auth::helpers::issue_tokens_and_cookie` persists the refresh row every
/// sign-in path shares; a denied insert there is the deployment's grant, not
/// an outage.
#[tokio::test]
async fn refresh_row_insert_denial_is_403_not_500() {
    let ctx = TestContext::with_auth_and_crypto().await;
    signup(&ctx, "persist@example.com").await;
    let ctx = denied(ctx, vec![("database.create", tokens::TABLE)]);
    assert_eq!(
        status(&ctx, login_msg(), login_body("persist@example.com")).await,
        403
    );
}

/// `auth::helpers::TokenGrant::resolve` reads the account's role grants for
/// the token it mints; a denial there is the deployment's grant, not an
/// outage, and must not read as "no roles" either.
#[tokio::test]
async fn role_grants_read_denial_is_403_not_500() {
    let ctx = TestContext::with_auth_and_crypto().await;
    signup(&ctx, "grants@example.com").await;
    let ctx = denied(
        ctx,
        vec![("database.list", crate::platform_state::user_roles::TABLE)],
    );
    assert_eq!(
        status(&ctx, login_msg(), login_body("grants@example.com")).await,
        403
    );
}

// --- api/signup.rs ---------------------------------------------------------

/// Signup finds out an address is taken by writing the account, so the
/// write is the one call a denial on signup can reach; it is the
/// deployment's grant, not an outage, and must not read as "registered"
/// either.
#[tokio::test]
async fn signup_account_write_denial_is_403_not_500() {
    let ctx = BatchDenied {
        inner: TestContext::with_auth_and_crypto().await,
        table: users::TABLE,
    };
    assert_eq!(
        status(
            &ctx,
            from_client(anon_msg("create", "/b/auth/api/signup")),
            login_body("new@example.com"),
        )
        .await,
        403
    );
}

// --- api/refresh.rs --------------------------------------------------------

#[tokio::test]
async fn refresh_token_lookup_denial_is_403_not_500() {
    let ctx = TestContext::with_auth_and_crypto().await;
    let signed_up = signup(&ctx, "refresh@example.com").await;
    let refresh_token = signed_up["refresh_token"]
        .as_str()
        .expect("an auto-login signup answers a refresh token")
        .to_string();
    let ctx = denied(ctx, vec![("database.list", tokens::TABLE)]);
    assert_eq!(
        status(
            &ctx,
            from_client(anon_msg("create", "/b/auth/api/refresh")),
            json!({"refresh_token": refresh_token}),
        )
        .await,
        403
    );
}

// --- api/me.rs -------------------------------------------------------------

#[tokio::test]
async fn me_read_denial_is_403_not_500() {
    let ctx = TestContext::with_auth_and_crypto().await;
    let signed_up = signup(&ctx, "me@example.com").await;
    let user_id = signed_up["user"]["id"]
        .as_str()
        .expect("user id")
        .to_string();
    let ctx = denied(ctx, vec![("database.get", users::TABLE)]);
    assert_eq!(
        status(
            &ctx,
            auth_msg("retrieve", "/b/auth/api/me", &user_id),
            serde_json::Value::Null,
        )
        .await,
        403
    );
}

/// `PATCH /me` for an account whose row is gone — deleted while its access
/// token was still live — is the 404 `GET /me` already answers, not a 500.
#[tokio::test]
async fn me_update_of_a_deleted_account_is_404_not_500() {
    let ctx = TestContext::with_auth_and_crypto().await;
    assert_eq!(
        status(
            &ctx,
            auth_msg("update", "/b/auth/api/me", "user-that-was-deleted"),
            json!({"name": "Renamed"}),
        )
        .await,
        404
    );
}

// --- api/api_keys.rs -------------------------------------------------------

#[tokio::test]
async fn api_key_list_denial_is_403_not_500() {
    let ctx = denied(
        TestContext::with_auth_and_crypto().await,
        vec![("database.list", api_keys::TABLE)],
    );
    assert_eq!(
        status(
            &ctx,
            auth_msg("retrieve", "/b/auth/api/api-keys", "user-a"),
            serde_json::Value::Null,
        )
        .await,
        403
    );
}

#[tokio::test]
async fn api_key_revoke_denial_is_403_not_500() {
    let ctx = TestContext::with_auth_and_crypto().await;
    let signed_up = signup(&ctx, "keys@example.com").await;
    let user_id = signed_up["user"]["id"]
        .as_str()
        .expect("user id")
        .to_string();
    let created = output_json(
        AuthUiBlock::default()
            .handle(
                &ctx,
                auth_msg("create", "/b/auth/api/api-keys", &user_id),
                InputStream::from_bytes(
                    serde_json::to_vec(&json!({"name": "ci"})).expect("serialize"),
                ),
            )
            .await,
    )
    .await;
    let id = created["id"].as_str().expect("created key id").to_string();
    let ctx = denied(ctx, vec![("database.update", api_keys::TABLE)]);
    assert_eq!(
        status(
            &ctx,
            auth_msg("update", &format!("/b/auth/api/api-keys/{id}"), &user_id),
            serde_json::Value::Null,
        )
        .await,
        403
    );
}

// --- api/logout.rs ---------------------------------------------------------

#[tokio::test]
async fn logout_revocation_denial_is_403_not_500() {
    let ctx = denied(
        TestContext::with_auth_and_crypto().await,
        vec![
            ("database.update_where", tokens::TABLE),
            ("database.update_where_count", tokens::TABLE),
        ],
    );
    assert_eq!(
        status(
            &ctx,
            auth_msg("create", "/b/auth/api/logout", "user-a"),
            serde_json::Value::Null,
        )
        .await,
        403
    );
}

// --- api/bootstrap.rs + auth/bootstrap.rs ----------------------------------

/// Redemption creates the admin through `auth::bootstrap`, which re-wrapped
/// every failure as `Internal` — a denied insert was a 500 however the
/// handler mapped it.
#[tokio::test]
async fn bootstrap_admin_insert_denial_is_403_not_500() {
    let ctx = TestContext::with_auth_and_crypto().await;
    let raw = "bootstrap-token-denied";
    let expires = (chrono::Utc::now() + chrono::Duration::hours(1))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    bootstrap_tokens::insert(
        &ctx,
        crate::blocks::auth::service::hash_token(raw),
        &expires,
    )
    .await
    .expect("seed a bootstrap token");

    let msg = from_client(anon_msg("create", "/b/auth/api/bootstrap"));
    let form = format!(
        "token={raw}&email=admin@example.com&password={PASSWORD}&csrf_token={}",
        crate::csrf::token(&ctx, &msg)
    );
    let ctx = BatchDenied {
        inner: ctx,
        table: users::TABLE,
    };
    let out = AuthUiBlock::default()
        .handle(&ctx, msg, InputStream::from_bytes(form.into_bytes()))
        .await;
    assert_eq!(output_http_status(out).await, 403);
}

// --- pages/orgs.rs ---------------------------------------------------------

#[tokio::test]
async fn orgs_page_read_denial_is_the_403_page_not_a_500() {
    let ctx = denied(
        TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID),
        vec![("database.list", orgs::TABLE)],
    );
    let mut msg = auth_msg("retrieve", "/b/auth/orgs", "user-a");
    msg.set_meta("http.header.accept", "text/html");
    let parts = wafer_block::http_codec::collect_http_response(
        AuthUiBlock::default()
            .handle(&ctx, msg, InputStream::empty())
            .await,
    )
    .await;
    let html = String::from_utf8_lossy(&parts.body);
    assert_eq!(parts.status, 403, "{html}");
    assert!(
        !html.contains("No claimed organizations"),
        "a refused read must not claim the user owns no orgs: {html}"
    );
}

#[tokio::test]
async fn orgs_page_read_denial_is_403_for_an_api_caller() {
    let ctx = denied(
        TestContext::with_auth()
            .await
            .running_as(crate::blocks::auth_ui::AUTH_UI_BLOCK_ID),
        vec![("database.list", orgs::TABLE)],
    );
    let mut msg = auth_msg("retrieve", "/b/auth/orgs", "user-a");
    msg.set_meta("http.header.accept", "application/json");
    assert_eq!(status(&ctx, msg, serde_json::Value::Null).await, 403);
}

// --- pages/settings.rs -----------------------------------------------------

/// The settings form reads every value through the config service, which
/// WRAP guards like the database: a deployment that never granted the block
/// its own settings gets the 403 page, not a 500 and not a form of defaults.
#[tokio::test]
async fn settings_page_config_denial_is_the_403_page_not_a_500() {
    let ctx = TestContext::with_auth().await.running_as("test/ungranted");
    let mut msg = crate::test_support::admin_msg("retrieve", "/b/auth/admin/settings");
    msg.set_meta("http.header.accept", "text/html");
    let parts = wafer_block::http_codec::collect_http_response(
        AuthUiBlock::default()
            .handle(&ctx, msg, InputStream::empty())
            .await,
    )
    .await;
    let html = String::from_utf8_lossy(&parts.body);
    assert_eq!(parts.status, 403, "{html}");
    assert!(!html.contains("settings-form"), "{html}");
}

// --- api/forgot_password.rs + api/verify.rs (resend) + api/login.rs --------
//
// The exception to this file's rule. A call that only a registered address
// reaches — the token stores behind forgot-password and resend-verification,
// and login's credentials read — must not answer with its own 403 or 500, or
// it tells an anonymous caller which addresses have accounts. They answer,
// whole response, what an unregistered address gets, and log the code instead.

/// Everything the HTTP boundary sends for one public request to `path`.
async fn wire(ctx: &dyn Context, path: &str, email: &str) -> (u16, Vec<(String, String)>, String) {
    let parts = wafer_block::http_codec::collect_http_response(
        AuthUiBlock::default()
            .handle(
                ctx,
                from_client(anon_msg("create", path)),
                InputStream::from_bytes(
                    serde_json::to_vec(&json!({ "email": email })).expect("serialize body"),
                ),
            )
            .await,
    )
    .await;
    (
        parts.status,
        parts.headers,
        String::from_utf8_lossy(&parts.body).into_owned(),
    )
}

/// A WRAP denial and an outage, the two refusals a write can meet.
fn write_refusals() -> [WaferError; 2] {
    [
        wrap_denial(),
        WaferError::new(ErrorCode::Internal, "simulated database outage"),
    ]
}

async fn seed_user(ctx: &TestContext, email: &str) {
    users::insert(
        ctx,
        users::NewUser {
            email: email.into(),
            display_name: "U".into(),
            avatar_url: None,
            role: "user".into(),
            email_verified: false,
            verification_token_hash: None,
        },
    )
    .await
    .expect("insert user");
}

#[tokio::test]
async fn forgot_password_reset_token_store_failure_answers_like_an_unregistered_address() {
    for error in write_refusals() {
        let ctx = TestContext::with_auth_and_crypto().await;
        seed_user(&ctx, "known@example.com").await;
        let failing = FailingDbOpContext::failing_with(
            ctx,
            vec![("database.update", users::TABLE)],
            error.clone(),
        );

        // The token is stored after the response; queue that work so the
        // store — and its failure — really runs below.
        crate::deferred::queue_for_test();
        let path = "/b/auth/api/forgot-password";
        let unregistered = wire(&failing, path, "nobody@example.com").await;
        let registered = wire(&failing, path, "known@example.com").await;

        assert_eq!(
            unregistered.0, 200,
            "the unregistered answer is the constant 200"
        );
        assert_eq!(
            registered, unregistered,
            "a {:?} storing the reset token must not be visible to the caller",
            error.code
        );
        assert_eq!(
            crate::blocks::auth_ui::api::run_deferred().await,
            1,
            "the failing store runs after the response, and fails there"
        );
    }
}

#[tokio::test]
async fn resend_verification_token_store_failure_answers_like_an_unregistered_address() {
    for error in write_refusals() {
        let ctx = TestContext::with_auth_and_crypto().await;
        seed_user(&ctx, "unproven@example.com").await;
        let failing = FailingDbOpContext::failing_with(
            ctx,
            vec![("database.update", users::TABLE)],
            error.clone(),
        );

        // The token is stored after the response; queue that work so the
        // store — and its failure — really runs below.
        crate::deferred::queue_for_test();
        let path = "/b/auth/api/resend-verification";
        let unregistered = wire(&failing, path, "nobody@example.com").await;
        let registered = wire(&failing, path, "unproven@example.com").await;

        assert_eq!(
            unregistered.0, 200,
            "the unregistered answer is the constant 200"
        );
        assert_eq!(
            registered, unregistered,
            "a {:?} storing the verification token must not be visible to the caller",
            error.code
        );
        assert_eq!(
            crate::blocks::auth_ui::api::run_deferred().await,
            1,
            "the failing store runs after the response, and fails there"
        );
    }
}

/// Everything the HTTP boundary sends for one login attempt as `email`.
async fn login_wire(ctx: &dyn Context, email: &str) -> (u16, Vec<(String, String)>, String) {
    let parts = wafer_block::http_codec::collect_http_response(
        AuthUiBlock::default()
            .handle(
                ctx,
                login_msg(),
                InputStream::from_bytes(
                    serde_json::to_vec(&login_body(email)).expect("serialize body"),
                ),
            )
            .await,
    )
    .await;
    (
        parts.status,
        parts.headers,
        String::from_utf8_lossy(&parts.body).into_owned(),
    )
}

#[tokio::test]
async fn login_credential_read_failure_answers_like_an_unregistered_address() {
    for error in write_refusals() {
        let ctx = TestContext::with_auth_and_crypto().await;
        signup(&ctx, "known@example.com").await;
        let failing = FailingDbOpContext::failing_with(
            ctx,
            vec![("database.list", local_credentials::TABLE)],
            error.clone(),
        );

        let unregistered = login_wire(&failing, "nobody@example.com").await;
        let registered = login_wire(&failing, "known@example.com").await;

        assert_eq!(
            unregistered.0, 401,
            "the unregistered answer is invalid credentials"
        );
        assert_eq!(
            registered, unregistered,
            "a {:?} reading the credential must not be visible to the caller",
            error.code
        );
    }
}
