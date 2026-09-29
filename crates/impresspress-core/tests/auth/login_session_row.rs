//! `handle_login` records a login family in `auth::repo::sessions` so the
//! userportal `/b/userportal/sessions` page lists the caller's devices.
//!
//! [B12] The property under test is that a row is a *device*, not an access
//! token: one login is one row, and however many times that login rotates its
//! tokens the row count stays at one. Before migration 012 every rotation
//! inserted a row, so a single tab wrote roughly forty-eight a day and the
//! page listed each as a separate device.
//!
//! These tests run on `common::auth_fixture` for its real `wafer-run/crypto`
//! block, so password hashing and JWT signing work the same way as
//! production.
//! Plan A2: `seed_password_user` applies migrations before inserting so the
//! typed schema (with `NOT NULL` constraints) is in place.

use impresspress_core::{
    blocks::{
        auth::{repo::sessions, AUTH_BLOCK_ID},
        auth_ui::AuthUiBlock,
        userportal::UserPortalBlock,
    },
    test_support::TestContext,
};
use serde_json::json;
use wafer_core::clients::crypto;
use wafer_run::{
    streams::output::{BufferedResponse, TerminalNotResponse},
    Block, ErrorCode, InputStream, Message, OutputStream, WaferError,
};

use crate::common::auth_fixture;

/// Drain an `OutputStream` to a `BufferedResponse`. Mirrors the helper in
/// `impresspress-core/src/test_support.rs` (which is `#[cfg(test)]` and not
/// visible from integration tests).
async fn collect_or_panic(out: OutputStream) -> BufferedResponse {
    match out.collect_buffered().await {
        Ok(buf) => buf,
        Err(TerminalNotResponse::Halt(buf)) => buf,
        Err(TerminalNotResponse::Error(e)) => {
            panic!("handler returned error: {} ({:?})", e.message, e.code)
        }
        Err(TerminalNotResponse::Drop { .. }) => panic!("handler dropped the request"),
        Err(TerminalNotResponse::Continue(_)) => panic!("handler returned Continue"),
        Err(TerminalNotResponse::Malformed) => panic!("handler returned malformed stream"),
    }
}

/// Seed a user with a local credential. Returns the user's id.
///
/// Applies Plan A2 migrations first (idempotent), then inserts the user row
/// and a `local_credentials` row via the typed repo helpers. `email_verified`
/// is set to `true` directly via `exec_raw` after insert so the login flow
/// doesn't gate on verification.
///
/// Plan A2 note: passwords live in `local_credentials`, not on the users row.
async fn seed_password_user(ctx: &TestContext, email: &str, password: &str) -> String {
    use impresspress_core::{
        blocks::auth::{migrations, repo::local_credentials},
        test_support::seed_user,
    };
    use wafer_core::clients::database as db;

    // Apply migrations so the typed schema (with NOT NULL constraints, etc.)
    // is in place before any inserts. Idempotent.
    migrations::apply(ctx).await.expect("migrations::apply");

    let password_hash = crypto::hash(ctx, password).await.expect("hash password");

    let user = seed_user(email).display_name("").insert(ctx).await;

    // Set email_verified via exec_raw (test-fixture setup — CLAUDE.md exception).
    db::exec_raw(
        &ctx.fixture(),
        "UPDATE wafer_run__auth__users SET email_verified = 1 WHERE id = ?",
        &[json!(&user.id)],
    )
    .await
    .expect("set email_verified");

    // Store the password in local_credentials.
    local_credentials::insert(ctx, &user.id, &password_hash, false)
        .await
        .expect("insert local_credentials");

    user.id
}

fn login_msg() -> Message {
    let mut m = Message::new("http.request");
    m.set_meta("req.action", "create");
    m.set_meta("req.resource", "/b/auth/api/login");
    m
}

async fn invoke_login(ctx: &TestContext, email: &str, password: &str) -> String {
    let block = AuthUiBlock::default();
    let body = json!({"email": email, "password": password}).to_string();
    let msg = login_msg();
    let out = block
        .handle(ctx, msg, InputStream::from_bytes(body.into_bytes()))
        .await;
    let buf = collect_or_panic(out).await;
    String::from_utf8(buf.body).expect("body utf8")
}

/// Run the login handler and return the error its stream terminates with,
/// panicking on any other terminal. The wrong-password test needs to know
/// the login was refused *as a bad credential*: a 500, 404 or 429 would also
/// write no session row, so "no row" alone proves nothing.
async fn invoke_login_expecting_error(
    ctx: &TestContext,
    email: &str,
    password: &str,
) -> WaferError {
    let block = AuthUiBlock::default();
    let body = json!({"email": email, "password": password}).to_string();
    let msg = login_msg();
    let out = block
        .handle(ctx, msg, InputStream::from_bytes(body.into_bytes()))
        .await;
    match out.collect_buffered().await {
        Err(TerminalNotResponse::Error(e)) => e,
        Ok(buf) => panic!(
            "login must be refused, got a response: {}",
            String::from_utf8_lossy(&buf.body)
        ),
        Err(other) => panic!("login must terminate with an error, got {other:?}"),
    }
}

/// The refresh token both handlers exchange, out of a login or refresh body.
fn refresh_token_of(body: &str) -> String {
    let resp: serde_json::Value =
        serde_json::from_str(body).unwrap_or_else(|_| panic!("body is not JSON: {body}"));
    resp.get("refresh_token")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("refresh_token missing: {body}"))
        .to_string()
}

/// The `family` claim on a refresh JWT, which is also the session row's key.
async fn family_of(ctx: &TestContext, refresh_token: &str) -> String {
    crypto::verify(ctx, refresh_token)
        .await
        .expect("verify refresh token")
        .get("family")
        .and_then(|v| v.as_str())
        .expect("refresh token carries a family claim")
        .to_string()
}

async fn invoke_refresh(ctx: &TestContext, refresh_token: &str) -> String {
    let block = AuthUiBlock::default();
    let body = json!({ "refresh_token": refresh_token }).to_string();
    let mut msg = Message::new("http.request");
    msg.set_meta("req.action", "create");
    msg.set_meta("req.resource", "/b/auth/api/refresh");
    let out = block
        .handle(ctx, msg, InputStream::from_bytes(body.into_bytes()))
        .await;
    let buf = collect_or_panic(out).await;
    String::from_utf8(buf.body).expect("body utf8")
}

#[tokio::test]
async fn login_creates_one_session_row_keyed_by_the_refresh_family() {
    let ctx = auth_fixture(impresspress_core::blocks::auth_ui::AUTH_UI_BLOCK_ID).await;
    let user_id = seed_password_user(&ctx, "alice@example.com", "hunter2hunter2").await;

    let resp_body = invoke_login(&ctx, "alice@example.com", "hunter2hunter2").await;
    let family = family_of(&ctx, &refresh_token_of(&resp_body)).await;

    let rows = sessions::list_for_user(&ctx, &user_id)
        .await
        .expect("list sessions");
    assert_eq!(
        rows.len(),
        1,
        "exactly one session row per login, got {}: {rows:?}",
        rows.len()
    );
    assert_eq!(
        rows[0].user_id, user_id,
        "session row must reference the logged-in user"
    );
    assert_eq!(
        rows[0].family, family,
        "the session row is keyed by the refresh rotation family"
    );
    assert_eq!(
        rows[0].auth_method, "password",
        "the row records how the session was established"
    );
}

/// The B12 regression. Three rotations of the same login leave one row,
/// touched — not four rows inserted. On the pre-012 tree this asserted four.
#[tokio::test]
async fn refreshing_touches_the_one_row_instead_of_inserting_more() {
    let ctx = auth_fixture(impresspress_core::blocks::auth_ui::AUTH_UI_BLOCK_ID).await;
    let user_id = seed_password_user(&ctx, "erin@example.com", "erin-password-1").await;

    let mut refresh =
        refresh_token_of(&invoke_login(&ctx, "erin@example.com", "erin-password-1").await);
    let family = family_of(&ctx, &refresh).await;

    for n in 1..=3 {
        let body = invoke_refresh(&ctx, &refresh).await;
        refresh = refresh_token_of(&body);
        assert_eq!(
            family_of(&ctx, &refresh).await,
            family,
            "rotation {n} must stay inside the same family (SEC-039)"
        );
        let rows = sessions::list_for_user(&ctx, &user_id)
            .await
            .expect("list sessions");
        assert_eq!(
            rows.len(),
            1,
            "after {n} rotation(s) the device must still be one row, got {}: {rows:?}",
            rows.len()
        );
        assert_eq!(rows[0].family, family);
    }
}

/// The session row expires with the refresh row it mirrors, not on a separate
/// 30-day clock. `SESSION_LIFETIME_DAYS` is the source of both.
#[tokio::test]
async fn the_session_row_expires_when_the_refresh_token_does() {
    use impresspress_core::blocks::auth::config::SESSION_LIFETIME_DAYS_DEFAULT;

    let ctx = auth_fixture(impresspress_core::blocks::auth_ui::AUTH_UI_BLOCK_ID).await;
    let user_id = seed_password_user(&ctx, "frank@example.com", "frank-password1").await;
    let body = invoke_login(&ctx, "frank@example.com", "frank-password1").await;
    let refresh = refresh_token_of(&body);

    let token_exp = crypto::verify(&ctx, &refresh)
        .await
        .expect("verify refresh token")
        .get("exp")
        .and_then(|v| v.as_i64())
        .expect("refresh token carries exp");

    let rows = sessions::list_for_user(&ctx, &user_id)
        .await
        .expect("list sessions");
    let row_exp = chrono::DateTime::parse_from_rfc3339(&rows[0].expires_at)
        .expect("row expiry is RFC 3339")
        .timestamp();

    assert!(
        (row_exp - token_exp).abs() <= 2,
        "the row must expire with the refresh token: row {row_exp}, token {token_exp}"
    );
    let expected =
        chrono::Utc::now().timestamp() + i64::from(SESSION_LIFETIME_DAYS_DEFAULT) * 86_400;
    assert!(
        (token_exp - expected).abs() <= 5,
        "refresh validity is SESSION_LIFETIME_DAYS ({SESSION_LIFETIME_DAYS_DEFAULT}) days:          got {token_exp}, expected about {expected}"
    );
}

#[tokio::test]
async fn invalid_credentials_do_not_create_a_session_row() {
    let ctx = auth_fixture(impresspress_core::blocks::auth_ui::AUTH_UI_BLOCK_ID).await;
    let user_id = seed_password_user(&ctx, "bob@example.com", "correct-horse").await;

    let err = invoke_login_expecting_error(&ctx, "bob@example.com", "WRONG-password").await;
    assert_eq!(
        err.code,
        ErrorCode::Unauthenticated,
        "a wrong password is a 401, not another failure: {} ({:?})",
        err.message,
        err.code
    );
    assert_eq!(
        err.detail_code(),
        Some("invalid_credentials"),
        "a wrong password is refused as a bad credential: {}",
        err.message
    );

    let rows = sessions::list_for_user(&ctx, &user_id)
        .await
        .expect("list sessions");
    assert!(
        rows.is_empty(),
        "no session row may be written for a failed login: {rows:?}"
    );

    // Control: the same user and fixture DO get a row once the password is
    // right, so the empty list above is the refusal, not a fixture that
    // could never have written one.
    let _ = invoke_login(&ctx, "bob@example.com", "correct-horse").await;
    let rows = sessions::list_for_user(&ctx, &user_id)
        .await
        .expect("list sessions");
    assert_eq!(
        rows.len(),
        1,
        "the correct password must write exactly one row: {rows:?}"
    );
}

/// Two logins are two devices. Each mints its own family, so each gets its
/// own row — no sleep needed, because the key is a random family rather than
/// a hash of a token whose `iat` only ticks once a second.
#[tokio::test]
async fn two_logins_produce_two_distinct_session_rows() {
    let ctx = auth_fixture(impresspress_core::blocks::auth_ui::AUTH_UI_BLOCK_ID).await;
    let user_id = seed_password_user(&ctx, "carol@example.com", "passw0rd-passw0rd").await;

    let _ = invoke_login(&ctx, "carol@example.com", "passw0rd-passw0rd").await;
    let _ = invoke_login(&ctx, "carol@example.com", "passw0rd-passw0rd").await;

    let rows = sessions::list_for_user(&ctx, &user_id)
        .await
        .expect("list sessions");
    assert_eq!(
        rows.len(),
        2,
        "two logins must produce two session rows, got {}",
        rows.len()
    );
    assert_ne!(
        rows[0].family, rows[1].family,
        "each login mints its own rotation family"
    );
}

/// End-to-end: after login writes a session row, the userportal sessions
/// page renders one row per active session (and a Revoke button for each).
/// This is what the user sees in their browser at `/b/userportal/sessions`.
#[tokio::test]
async fn userportal_sessions_page_renders_row_after_login() {
    let ctx = auth_fixture(impresspress_core::blocks::auth_ui::AUTH_UI_BLOCK_ID).await;
    let user_id = seed_password_user(&ctx, "diana@example.com", "diana-password").await;

    let _ = invoke_login(&ctx, "diana@example.com", "diana-password").await;

    let block = UserPortalBlock::new();
    let mut msg = Message::new("http.request");
    msg.set_meta("req.action", "retrieve");
    msg.set_meta("req.resource", "/b/userportal/sessions");
    msg.set_meta("auth.user_id", &user_id);
    let out = block.handle(&ctx, msg, InputStream::empty()).await;
    let buf = collect_or_panic(out).await;
    let html = String::from_utf8(buf.body).expect("body utf8");

    // Title moved to Topbar crumb + subtitle (see ui(pages) commit that
    // moved page-header content into the topbar).
    assert!(
        html.contains("<h1 class=\"account-card__title\">Sessions</h1>"),
        "page must render the Sessions header: {html}"
    );
    assert!(
        html.contains(">Revoke<"),
        "populated page must render at least one Revoke button: {html}"
    );
    assert!(
        !html.contains("No active sessions"),
        "populated page must not show the empty state: {html}"
    );
}

/// Sanity check: `AUTH_BLOCK_ID` is the block name we'd register at runtime.
/// If this drifts, every other test in this file is testing the wrong
/// surface.
#[tokio::test]
async fn auth_block_id_is_what_we_target() {
    assert_eq!(AUTH_BLOCK_ID, "wafer-run/auth");
}
