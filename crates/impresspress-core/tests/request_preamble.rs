//! The request preamble, driven the way a client reaches it.
//!
//! Every request here goes through [`TestContext::request`]: the production
//! router block resolves the credential off the `Authorization` header or
//! the `auth_token` cookie, `pipeline::handle_request` verifies it and writes
//! the caller's identity, and the CSRF origin policy judges a
//! cookie-authenticated write — before any block runs. Every credential is
//! minted by the real routes: a session by `POST /b/auth/api/login`, an API
//! key by `POST /b/auth/api/api-keys`.
//!
//! Each refusal below is one a test with the caller injected as `auth.*`
//! meta cannot see: the injected identity is believed as it stands, so a
//! cross-site form, a signed-out token and an API key whose owner is no
//! admin all reach the handler as whoever the test said they were.

#![cfg(feature = "block-userportal")]

use std::sync::Arc;

use impresspress_core::{
    blocks::{admin::AdminBlock, auth::repo::users, userportal::UserPortalBlock},
    test_support::{anon_msg, api_key_header, Session, TestContext},
};
use wafer_block::http_codec::{collect_http_response, HttpResponseParts};
use wafer_run::{InputStream, Message};

const PASSWORD: &str = "correct-horse-battery-staple";

/// A deployment with sign-in, the admin block and the user portal.
async fn deployment() -> TestContext {
    let mut ctx = TestContext::with_userportal().await.with_sign_in_added();
    ctx.register_block("impresspress/admin", Arc::new(AdminBlock::new()));
    ctx.register_block("impresspress/userportal", Arc::new(UserPortalBlock::new()));
    ctx
}

/// Seed an account with `role` and sign it in through the login route.
async fn signed_in(ctx: &TestContext, email: &str, role: &str) -> Session {
    ctx.seed_account(email, PASSWORD, role).await;
    ctx.sign_in(email, PASSWORD).await
}

/// The HTTP response `msg` sent through the preamble with `body` comes to.
async fn respond(ctx: &TestContext, msg: Message, body: &str) -> HttpResponseParts {
    let out = ctx
        .request_with_input(msg, InputStream::from_bytes(body.as_bytes().to_vec()))
        .await;
    collect_http_response(out).await
}

/// Status and body of `msg` sent through the preamble with `body`.
async fn send(ctx: &TestContext, msg: Message, body: &str) -> (u16, String) {
    let parts = respond(ctx, msg, body).await;
    (
        parts.status,
        String::from_utf8_lossy(&parts.body).into_owned(),
    )
}

/// The value of response header `name`, matched case-insensitively.
fn header<'a>(parts: &'a HttpResponseParts, name: &str) -> Option<&'a str> {
    parts
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// `GET /b/auth/api/me` with `session`'s bearer token.
async fn me(ctx: &TestContext, session: &Session) -> HttpResponseParts {
    respond(
        ctx,
        session.bearer(anon_msg("retrieve", "/b/auth/api/me")),
        "",
    )
    .await
}

/// The router's answer to an API caller with no identity on an
/// authenticated route (`routing::check_access` →
/// `ui::unauthenticated_response`): what a credential the preamble refused
/// comes to. `401` with the challenge — the status a client reads as "sign
/// in" — never the `403` that means "signed in, but not allowed".
fn assert_anonymous(parts: &HttpResponseParts) {
    let body = String::from_utf8_lossy(&parts.body);
    assert_eq!(parts.status, 401, "{body}");
    assert_eq!(
        header(parts, "WWW-Authenticate"),
        Some(r#"Bearer realm="impresspress", ApiKey realm="impresspress""#),
        "{body}"
    );
    assert!(body.contains("authentication required"), "{body}");
}

/// The admin users page's Disable button: an htmx POST, which carries no
/// form token — the origin policy is the whole of its CSRF defence.
fn disable_click(admin: &Session, user_id: &str) -> Message {
    let mut msg = admin.cookie(anon_msg(
        "create",
        &format!("/b/admin/users/{user_id}/disable"),
    ));
    msg.set_meta("http.header.hx-request", "true");
    msg.set_meta("http.header.host", "impresspress.example.com");
    msg
}

async fn is_disabled(ctx: &TestContext, user_id: &str) -> bool {
    users::find_by_id(&ctx.fixture(), user_id)
        .await
        .expect("read the user")
        .expect("the user exists")
        .disabled
}

/// A cross-site page riding the admin's cookie cannot disable an account.
/// With the admin injected as meta this POST reaches the handler and the
/// account is disabled: the origin policy only runs for a credential the
/// router took off the cookie.
#[tokio::test]
async fn a_cross_site_cookie_post_is_refused_before_the_admin_form_runs() {
    let ctx = deployment().await;
    let admin = signed_in(&ctx, "admin@example.com", "admin").await;
    let member = signed_in(&ctx, "member@example.com", "user").await;

    let mut msg = disable_click(&admin, &member.user_id);
    msg.set_meta("http.header.sec-fetch-site", "cross-site");
    let (status, body) = send(&ctx, msg, "").await;

    assert_eq!(status, 403, "{body}");
    assert!(!is_disabled(&ctx, &member.user_id).await);
    assert_eq!(me(&ctx, &member).await.status, 200);
}

/// With no Fetch-Metadata, `Origin` or `Referer` there is no evidence the
/// POST came from this site, and the policy fails closed.
#[tokio::test]
async fn a_cookie_post_with_no_origin_evidence_is_refused() {
    let ctx = deployment().await;
    let admin = signed_in(&ctx, "admin@example.com", "admin").await;
    let member = signed_in(&ctx, "member@example.com", "user").await;

    let (status, body) = send(&ctx, disable_click(&admin, &member.user_id), "").await;

    assert_eq!(status, 403, "{body}");
    assert!(!is_disabled(&ctx, &member.user_id).await);
}

/// The control both refusals above depart from: the same click from the
/// admin's own page is admitted, and the disabled member's already-issued
/// token stops working on its next request — the preamble's
/// `auth_version` check, which only a real token exercises.
#[tokio::test]
async fn a_same_origin_click_disables_the_account_and_its_live_token() {
    let ctx = deployment().await;
    let admin = signed_in(&ctx, "admin@example.com", "admin").await;
    let member = signed_in(&ctx, "member@example.com", "user").await;
    assert_eq!(me(&ctx, &member).await.status, 200, "precondition");

    let mut msg = disable_click(&admin, &member.user_id);
    msg.set_meta("http.header.sec-fetch-site", "same-origin");
    let (status, body) = send(&ctx, msg, "").await;

    assert_eq!(status, 200, "{body}");
    assert!(is_disabled(&ctx, &member.user_id).await);
    assert_anonymous(&me(&ctx, &member).await);
}

/// The profile form's `csrf_token` field, as `GET /b/userportal/profile`
/// renders it for the session's cookie.
async fn rendered_form_token(ctx: &TestContext, session: &Session) -> String {
    let mut msg = session.cookie(anon_msg("retrieve", "/b/userportal/profile"));
    msg.set_meta("http.header.accept", "text/html");
    let (status, html) = send(ctx, msg, "").await;
    assert_eq!(status, 200, "{html}");
    let after = html
        .split(r#"name="csrf_token" value=""#)
        .nth(1)
        .expect("the profile page embeds the form token");
    after[..after.find('"').expect("closing quote")].to_string()
}

fn profile_post(session: &Session, fetch_site: &str) -> Message {
    let mut msg = session.cookie(anon_msg("create", "/b/userportal/update-profile"));
    msg.set_meta("http.header.accept", "text/html");
    msg.set_meta("http.header.sec-fetch-site", fetch_site);
    msg
}

async fn display_name(ctx: &TestContext, user_id: &str) -> String {
    users::find_by_id(&ctx.fixture(), user_id)
        .await
        .expect("read the user")
        .expect("the user exists")
        .display_name
}

/// The profile form's own token check, keyed to the identity the preamble
/// resolved from the cookie: a same-origin POST without the token is
/// refused, and the one the page rendered is accepted.
#[tokio::test]
async fn the_profile_form_needs_the_token_its_page_rendered() {
    let ctx = deployment().await;
    let member = signed_in(&ctx, "member@example.com", "user").await;
    let before = display_name(&ctx, &member.user_id).await;

    let (status, body) = send(
        &ctx,
        profile_post(&member, "same-origin"),
        "name=Forged+Name",
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(display_name(&ctx, &member.user_id).await, before);

    let token = rendered_form_token(&ctx, &member).await;
    let (status, body) = send(
        &ctx,
        profile_post(&member, "same-origin"),
        &format!("csrf_token={token}&name=New+Name"),
    )
    .await;
    assert_eq!(status, 303, "{body}");
    assert_eq!(display_name(&ctx, &member.user_id).await, "New Name");
}

/// Even the page's own token does not carry a cross-site POST: the origin
/// policy refuses it before the form's check runs.
#[tokio::test]
async fn a_cross_site_profile_post_is_refused_even_with_the_token() {
    let ctx = deployment().await;
    let member = signed_in(&ctx, "member@example.com", "user").await;
    let before = display_name(&ctx, &member.user_id).await;
    let token = rendered_form_token(&ctx, &member).await;

    let (status, body) = send(
        &ctx,
        profile_post(&member, "cross-site"),
        &format!("csrf_token={token}&name=Forged+Name"),
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(display_name(&ctx, &member.user_id).await, before);
}

/// Signing out blocklists the access token presented with the sign-out, and
/// the preamble refuses it from then on. With the caller injected as meta,
/// sign-out has no `jti` to blocklist and the next request is admitted
/// regardless.
#[tokio::test]
async fn a_signed_out_access_token_is_refused() {
    let ctx = deployment().await;
    let member = signed_in(&ctx, "member@example.com", "user").await;
    assert_eq!(me(&ctx, &member).await.status, 200, "precondition");

    let (status, body) = send(
        &ctx,
        member.bearer(anon_msg("create", "/b/auth/api/logout")),
        "",
    )
    .await;
    assert_eq!(status, 303, "{body}");

    assert_anonymous(&me(&ctx, &member).await);
}

/// An API key authenticates as its owner with its owner's roles — not
/// more. A member's key is refused an admin route the member is refused.
#[tokio::test]
async fn an_api_key_is_refused_a_route_above_its_owners_role() {
    let ctx = deployment().await;
    let member = signed_in(&ctx, "member@example.com", "user").await;
    let key = member.create_api_key(&ctx, "member key").await;

    let (status, body) = send(
        &ctx,
        api_key_header(anon_msg("retrieve", "/b/admin/api/iam/roles"), &key),
        "",
    )
    .await;
    assert_eq!(status, 403, "{body}");
    assert!(!body.contains("authentication required"), "{body}");

    // The same key is the member on a route the member may use.
    let (status, body) = send(
        &ctx,
        api_key_header(anon_msg("retrieve", "/b/auth/api/me"), &key),
        "",
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(&member.user_id), "{body}");
}

/// The control for the refusal above: an admin's key reaches the admin
/// route.
#[tokio::test]
async fn an_admins_api_key_reaches_an_admin_route() {
    let ctx = deployment().await;
    let admin = signed_in(&ctx, "admin@example.com", "admin").await;
    let key = admin.create_api_key(&ctx, "admin key").await;

    let (status, body) = send(
        &ctx,
        api_key_header(anon_msg("retrieve", "/b/admin/api/iam/roles"), &key),
        "",
    )
    .await;
    assert_eq!(status, 200, "{body}");
}

/// A key that matches no row is no credential: the request continues as
/// anonymous and an authenticated route refuses it.
#[tokio::test]
async fn an_unknown_api_key_is_anonymous() {
    let ctx = deployment().await;

    assert_anonymous(
        &respond(
            &ctx,
            api_key_header(anon_msg("retrieve", "/b/auth/api/me"), "sb_not-a-key"),
            "",
        )
        .await,
    );
}

/// An API call carrying no credential at all is told to sign in: the
/// `401` the SDK's `getUser()` reads as "signed out" and returns `null` for.
#[tokio::test]
async fn an_anonymous_api_call_is_401_with_a_challenge() {
    let ctx = deployment().await;

    assert_anonymous(&respond(&ctx, anon_msg("retrieve", "/b/auth/api/me"), "").await);
    assert_anonymous(&respond(&ctx, anon_msg("retrieve", "/b/admin/api/iam/roles"), "").await);
}

/// The response the JS SDK's `getUser()` test replays
/// (`packages/impresspress-js/test/anonymous-me.response.json`), as the
/// server sends it to an SDK call with no session: status, the two headers
/// the SDK reads, and the body byte for byte. The SDK test is only worth
/// what this keeps true — its mock is these bytes, not a guess at them.
#[tokio::test]
async fn the_sdk_fixture_is_what_an_anonymous_me_answers() {
    let ctx = deployment().await;
    let parts = respond(&ctx, anon_msg("retrieve", "/b/auth/api/me"), "").await;

    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packages/impresspress-js/test/anonymous-me.response.json"
    );
    let fixture: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read the SDK fixture"))
            .expect("the SDK fixture is JSON");
    let actual = serde_json::json!({
        "status": parts.status,
        "headers": {
            "content-type": header(&parts, "Content-Type"),
            "www-authenticate": header(&parts, "WWW-Authenticate"),
        },
        "body": String::from_utf8_lossy(&parts.body),
    });
    assert_eq!(
        actual,
        fixture,
        "the SDK fixture no longer matches the server; write this into it:\n{}",
        serde_json::to_string_pretty(&actual).expect("serialize")
    );
}

/// A signed-in member on an admin route is identified and not permitted:
/// `403`, with no challenge, because signing in again would not help.
#[tokio::test]
async fn a_signed_in_member_on_an_admin_route_is_403() {
    let ctx = deployment().await;
    let member = signed_in(&ctx, "member@example.com", "user").await;

    let parts = respond(
        &ctx,
        member.bearer(anon_msg("retrieve", "/b/admin/api/iam/roles")),
        "",
    )
    .await;
    let body = String::from_utf8_lossy(&parts.body);
    assert_eq!(parts.status, 403, "{body}");
    assert_eq!(header(&parts, "WWW-Authenticate"), None, "{body}");
}

/// A browser page is still sent to the login form, with a return path, when
/// the visitor has no session or the one its cookie carries was signed out.
#[tokio::test]
async fn an_anonymous_page_request_is_sent_to_login() {
    let ctx = deployment().await;
    let member = signed_in(&ctx, "member@example.com", "user").await;
    let (status, body) = send(
        &ctx,
        member.bearer(anon_msg("create", "/b/auth/api/logout")),
        "",
    )
    .await;
    assert_eq!(status, 303, "{body}");

    for msg in [
        anon_msg("retrieve", "/b/userportal/profile"),
        member.cookie(anon_msg("retrieve", "/b/userportal/profile")),
    ] {
        let mut msg = msg;
        msg.set_meta("http.header.accept", "text/html");
        let parts = respond(&ctx, msg, "").await;
        assert_eq!(parts.status, 302);
        assert_eq!(
            header(&parts, "Location"),
            Some("/b/auth/login?redirect=%2Fb%2Fuserportal%2Fprofile")
        );
        assert_eq!(header(&parts, "WWW-Authenticate"), None);
    }
}

/// `request` sends what the wire carries; a message whose caller a test
/// already wrote as `auth.*` meta is the other harness path's business.
#[tokio::test]
#[should_panic(expected = "the pipeline writes the auth.* meta")]
async fn request_refuses_a_message_with_an_injected_identity() {
    let ctx = deployment().await;
    ctx.request(impresspress_core::test_support::admin_msg(
        "retrieve",
        "/b/admin/api/iam/roles",
    ))
    .await;
}
